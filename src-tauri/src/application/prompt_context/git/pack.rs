//! Bound the work of indexing and reading ordinary SHA-1 Git packs before
//! libgit2 receives them. Inflation uses fixed scratch space. Every delta
//! program and its reconstructed object count against the expanded budget.

use std::collections::BTreeSet;

use flate2::{Decompress, FlushDecompress, Status};

use super::super::ContextError;

pub(super) const MAX_OBJECTS: u32 = 64 * 1024;
const SHA1_BYTES: usize = 20;
const PACK_HEADER_BYTES: usize = 12;

pub(super) fn validate(
    bytes: &[u8],
    expanded_budget: &mut u64,
    object_budget: &mut u32,
) -> Result<(), ContextError> {
    let data_len = bytes
        .len()
        .checked_sub(SHA1_BYTES)
        .ok_or(ContextError::ReadUnavailable)?;
    let contents = &bytes[..data_len];
    let (header, mut input) = contents
        .split_at_checked(PACK_HEADER_BYTES)
        .ok_or(ContextError::ReadUnavailable)?;
    let [b'P', b'A', b'C', b'K', 0, 0, 0, 2 | 3, count @ ..] = header else {
        return Err(ContextError::ReadUnavailable);
    };
    let count = u32::from_be_bytes(
        count
            .try_into()
            .map_err(|_| ContextError::ReadUnavailable)?,
    );
    *object_budget = object_budget
        .checked_sub(count)
        .ok_or(ContextError::Limit)?;
    let mut offsets = BTreeSet::new();
    for _ in 0..count {
        let offset = contents.len() - input.len();
        let first = next_byte(&mut input)?;
        let kind = (first >> 4) & 7;
        let size = size_after(first, u64::from(first & 15), 4, &mut input)?;
        charge(expanded_budget, size)?;
        match kind {
            1..=4 => {}
            6 => {
                let base = offset_base(&mut input, offset)?;
                if !offsets.contains(&base) {
                    return Err(ContextError::ReadUnavailable);
                }
            }
            7 => {
                input = input
                    .get(SHA1_BYTES..)
                    .ok_or(ContextError::ReadUnavailable)?;
            }
            _ => return Err(ContextError::ReadUnavailable),
        }
        let (compressed, prefix, prefix_len) = inflate(input, size)?;
        if matches!(kind, 6 | 7) {
            let mut prefix = &prefix[..prefix_len];
            // The base is another admitted object in this same self-contained
            // pack. libgit2 also checks the declared base size against it.
            delta_size(&mut prefix)?;
            charge(expanded_budget, delta_size(&mut prefix)?)?;
        }
        input = &input[compressed..];
        offsets.insert(offset);
    }
    if !input.is_empty() {
        return Err(ContextError::ReadUnavailable);
    }
    Ok(())
}

fn charge(budget: &mut u64, amount: u64) -> Result<(), ContextError> {
    *budget = budget.checked_sub(amount).ok_or(ContextError::Limit)?;
    Ok(())
}

fn next_byte(input: &mut &[u8]) -> Result<u8, ContextError> {
    let (&byte, tail) = input.split_first().ok_or(ContextError::ReadUnavailable)?;
    *input = tail;
    Ok(byte)
}

fn size_after(
    mut byte: u8,
    mut size: u64,
    mut shift: u32,
    input: &mut &[u8],
) -> Result<u64, ContextError> {
    while byte & 128 != 0 {
        byte = next_byte(input)?;
        let bits = u64::from(byte & 127);
        if shift >= u64::BITS || bits > (u64::MAX >> shift) {
            return Err(ContextError::Limit);
        }
        size |= bits << shift;
        shift += 7;
    }
    Ok(size)
}

fn delta_size(input: &mut &[u8]) -> Result<u64, ContextError> {
    let byte = next_byte(input)?;
    size_after(byte, u64::from(byte & 127), 7, input)
}

fn offset_base(input: &mut &[u8], offset: usize) -> Result<usize, ContextError> {
    let mut byte = next_byte(input)?;
    let mut distance = usize::from(byte & 127);
    while byte & 128 != 0 {
        byte = next_byte(input)?;
        distance = distance
            .checked_add(1)
            .and_then(|prior| prior.checked_mul(128))
            .and_then(|prefix| prefix.checked_add(usize::from(byte & 127)))
            .filter(|total| *total < offset)
            .ok_or(ContextError::ReadUnavailable)?;
    }
    offset
        .checked_sub(distance)
        .ok_or(ContextError::ReadUnavailable)
}

fn inflate(input: &[u8], size: u64) -> Result<(usize, [u8; 20], usize), ContextError> {
    let mut decoder = Decompress::new(true);
    let mut buffer = [0; 8 * 1024];
    let mut prefix = [0; 20];
    let mut prefix_len = 0;
    loop {
        let consumed = usize::try_from(decoder.total_in()).map_err(|_| ContextError::Limit)?;
        let produced = decoder.total_out();
        let status = decoder
            .decompress(&input[consumed..], &mut buffer, FlushDecompress::None)
            .map_err(|_| ContextError::ReadUnavailable)?;
        if decoder.total_out() > size {
            return Err(ContextError::Limit);
        }
        let written =
            usize::try_from(decoder.total_out() - produced).map_err(|_| ContextError::Limit)?;
        let copy = written.min(prefix.len() - prefix_len);
        prefix[prefix_len..prefix_len + copy].copy_from_slice(&buffer[..copy]);
        prefix_len += copy;
        if status == Status::StreamEnd && decoder.total_out() == size {
            let compressed =
                usize::try_from(decoder.total_in()).map_err(|_| ContextError::Limit)?;
            return Ok((compressed, prefix, prefix_len));
        }
        if status == Status::StreamEnd || (decoder.total_in() == consumed as u64 && written == 0) {
            return Err(ContextError::ReadUnavailable);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_pack_respects_inflated_byte_and_object_budgets() {
        let path = std::env::temp_dir().join(format!("nexora-pack-test-{}", uuid::Uuid::new_v4()));
        let repo = git2::Repository::init(&path).unwrap();
        let content = "ordinary repeated text\n".repeat(1000);
        let revised = format!("{content}A new paragraph.\n");
        let first = repo.blob(content.as_bytes()).unwrap();
        let second = repo.blob(revised.as_bytes()).unwrap();
        let mut builder = repo.packbuilder().unwrap();
        builder.insert_object(first, Some("example.txt")).unwrap();
        builder.insert_object(second, Some("example.txt")).unwrap();
        let mut bytes = git2::Buf::new();
        builder.write_buf(&mut bytes).unwrap();

        assert!(bytes.len() < content.len());
        let raw_size = (content.len() + revised.len()) as u64;
        let baseline = raw_size + 1024;
        let mut budget = baseline;
        assert!(validate(&bytes, &mut budget, &mut 2).is_ok());
        let charged = baseline - budget;
        assert!(
            charged > raw_size,
            "a normal delta also charges its program"
        );
        assert!(matches!(
            validate(&bytes, &mut (charged - 1), &mut 2),
            Err(ContextError::Limit)
        ));
        budget = baseline;
        assert!(matches!(
            validate(&bytes, &mut budget, &mut 1),
            Err(ContextError::Limit)
        ));
        drop(builder);
        drop(repo);
        std::fs::remove_dir_all(path).unwrap();
    }
}
