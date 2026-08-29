//! Spend-guard pricing policy (Task 4.3).
//!
//! A single documented conservative default rate is applied to **every**
//! model. This is a deliberate policy placeholder — not provider rate data —
//! and is adjustable when the Phase 5 settings surface lands.
//!
//! Units are integer micro-USD (`u64`): 1 USD = `1_000_000` micro-USD.  Cost
//! math uses `u128` intermediates and saturates, so no float is ever involved
//! and overflow is infallible.  Token -> USD conversion uses ceiling
//! division, so a guardrail over-accounts rather than under-accounts.
//!
//! See `docs/AGENT-4.3-DESIGN.md` and `docs/DATABASE.md` section 7.8.

use crate::application::execution::TokenUsage;

/// Policy default: input price per 1M tokens, micro-USD.
///
/// Conservative placeholder pending Phase 5.  Adjust here when settings land.
pub(crate) const POLICY_DEFAULT_INPUT_MICRO_PER_1M: u64 = 5_000_000;

/// Policy default: output price per 1M tokens, micro-USD.
pub(crate) const POLICY_DEFAULT_OUTPUT_MICRO_PER_1M: u64 = 25_000_000;

/// Compute the billed cost for `input_tokens` / `output_tokens` at the policy
/// rate, in micro-USD, ceiling-rounded per 1M-token block.
///
/// `u128` is used for the product `tokens * price_per_1m` to avoid `u64`
/// overflow (`u64::MAX * 25_000_000` does not fit in `u64`); the result is
/// clamped to `u64::MAX` via saturating semantics.
#[must_use]
pub(crate) fn cost_micro(input_tokens: u64, output_tokens: u64) -> u64 {
    let in_cost = ceil_cost(input_tokens, POLICY_DEFAULT_INPUT_MICRO_PER_1M);
    let out_cost = ceil_cost(output_tokens, POLICY_DEFAULT_OUTPUT_MICRO_PER_1M);
    in_cost.saturating_add(out_cost)
}

/// Convenience wrapper for [`TokenUsage`].
#[must_use]
pub(crate) fn cost_for_usage(usage: TokenUsage) -> u64 {
    cost_micro(usage.input_tokens, usage.output_tokens)
}

fn ceil_cost(tokens: u64, price_per_1m: u64) -> u64 {
    if tokens == 0 || price_per_1m == 0 {
        return 0;
    }
    let product = u128::from(tokens) * u128::from(price_per_1m);
    // Ceiling division by `1_000_000`
    let ceil = product.div_ceil(1_000_000);
    if ceil > u128::from(u64::MAX) {
        u64::MAX
    } else {
        #[allow(clippy::cast_possible_truncation)]
        {
            ceil as u64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_tokens_costs_zero() {
        assert_eq!(cost_micro(0, 0), 0);
        assert_eq!(
            cost_micro(0, 100),
            ceil_cost(100, POLICY_DEFAULT_OUTPUT_MICRO_PER_1M)
        );
        assert_eq!(
            cost_micro(100, 0),
            ceil_cost(100, POLICY_DEFAULT_INPUT_MICRO_PER_1M)
        );
    }

    #[test]
    fn exact_1m_block_costs_exact_price() {
        // 1_000_000 tokens at the default rate costs exactly the per-1M price
        assert_eq!(cost_micro(1_000_000, 0), POLICY_DEFAULT_INPUT_MICRO_PER_1M);
        assert_eq!(cost_micro(0, 1_000_000), POLICY_DEFAULT_OUTPUT_MICRO_PER_1M);
        assert_eq!(
            cost_micro(1_000_000, 1_000_000),
            POLICY_DEFAULT_INPUT_MICRO_PER_1M + POLICY_DEFAULT_OUTPUT_MICRO_PER_1M
        );
    }

    #[test]
    fn rounding_is_ceiling_not_floor() {
        // 1 token: product = 1 * 5_000_000 = 5_000_000; /1_000_000 = 5.0 -> 5 micro
        // But with ceiling, even 1 token costs ceil(5_000_000 / 1_000_000) = 5
        // For a contrived price that doesn't divide evenly:
        // Use the real policy: 5_000_000 * 1 /1_000_000 = 5 exactly, so not illustrative.
        // Instead test that a fractional cent rounds up: 500_000 tokens * 5_000_000 = 2_500_000_000_000
        // /1_000_000 = 2_500_000 exactly, also even. Let's test the ceiling property directly:
        let one_token_input = cost_micro(1, 0);
        assert_eq!(
            one_token_input, 5,
            "1 input token must cost 5 micro (ceiling of 5)"
        );
        // 1_000_001 tokens: (1_000_001*5_000_000)/1_000_000 = 5_000_005.0 -> ceil 5_000_005
        // floor would be 5_000_005 as well (since .0? Actually 1_000_001*5_000_000 =5_000_005_000_000 /1M=5_000_005)
        // Need a case where product not divisible: use 1 token with price 3 would be 3/1M -> ceil 1? But our price is divisible.
        // Instead assert ceiling division helper directly:
        assert_eq!(ceil_cost(1, 1), 1, "1*1/1M ceil =1");
        assert_eq!(ceil_cost(1, 500_000), 1, "500k/1M ceil=1, floor=0");
        assert_eq!(ceil_cost(1_000_000, 1), 1);
        assert_eq!(ceil_cost(1_000_001, 1), 2);
    }

    #[test]
    fn cost_for_usage_wraps_cost_micro() {
        let usage = TokenUsage {
            input_tokens: 2_000,
            output_tokens: 3_000,
        };
        assert_eq!(cost_for_usage(usage), cost_micro(2_000, 3_000));
    }

    #[test]
    fn saturating_on_overflow() {
        // u64::MAX tokens * max price would overflow u64, but must saturate
        let huge = cost_micro(u64::MAX, u64::MAX);
        assert_eq!(huge, u64::MAX, "huge cost must saturate to u64::MAX");
        let also_huge = ceil_cost(u64::MAX, POLICY_DEFAULT_INPUT_MICRO_PER_1M);
        assert_eq!(also_huge, u64::MAX);
    }
}
