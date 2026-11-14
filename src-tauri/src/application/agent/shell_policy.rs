//! Bounded native shell analysis. Classification never authorizes execution.
use serde::Serialize;
use tree_sitter::{Node, Parser};
const MAX_BYTES: usize = 32 * 1024;
const MAX_NODES: usize = 16_384;
const MAX_DEPTH: usize = 16;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) enum ShellCategory {
    GeneralShell,
    DependencyInstall,
    PublishOrRemote,
    DestructiveGit,
    MassDelete,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ShellAnalysis {
    pub category: ShellCategory,
    pub packages: Vec<String>,
    pub packages_exact: bool,
    pub uncertain: bool,
    pub sandbox_posture: &'static str,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shell {
    Sh,
    Pwsh,
    Cmd,
}
impl Shell {
    pub(crate) fn platform() -> Self {
        if cfg!(windows) {
            Self::Cmd
        } else {
            Self::Sh
        }
    }
}
impl ShellAnalysis {
    fn new() -> Self {
        Self {
            category: ShellCategory::GeneralShell,
            packages: Vec::new(),
            packages_exact: true,
            uncertain: false,
            sandbox_posture: "PerCommandConsent",
        }
    }
    fn merge(&mut self, other: Self) {
        self.category = self.category.max(other.category);
        self.uncertain |= other.uncertain;
        self.packages_exact &= other.packages_exact;
        self.packages.extend(other.packages);
    }
}
pub(crate) fn analyze(command: &str, shell: Shell) -> ShellAnalysis {
    let mut result = analyze_inner(command, shell, 0);
    result.packages.sort();
    result.packages.dedup();
    result
}
fn analyze_inner(command: &str, shell: Shell, depth: usize) -> ShellAnalysis {
    let mut result = ShellAnalysis::new();
    if command.len() > MAX_BYTES || depth > MAX_DEPTH || command.contains('\0') {
        result.uncertain = true;
        result.packages_exact = false;
        return result;
    }
    if shell == Shell::Cmd {
        analyze_cmd(command, depth, &mut result);
        return result;
    }
    let mut parser = Parser::new();
    let language = match shell {
        Shell::Sh => tree_sitter_bash::LANGUAGE.into(),
        Shell::Pwsh => tree_sitter_powershell::LANGUAGE.into(),
        Shell::Cmd => unreachable!(),
    };
    if parser.set_language(&language).is_err() {
        result.uncertain = true;
        result.packages_exact = false;
        return result;
    }
    // Input is capped above. The progress callback additionally bounds parser work.
    let mut polls = 0usize;
    let mut progress = |_: &tree_sitter::ParseState| {
        polls += 1;
        polls > MAX_NODES
    };
    let options = tree_sitter::ParseOptions::new().progress_callback(&mut progress);
    let mut input =
        |offset: usize, _: tree_sitter::Point| command.as_bytes().get(offset..).unwrap_or_default();
    let Some(tree) = parser.parse_with_options(&mut input, None, Some(options)) else {
        result.uncertain = true;
        result.packages_exact = false;
        return result;
    };
    result.uncertain = tree.root_node().has_error();
    let mut stack = vec![tree.root_node()];
    let mut count = 0;
    while let Some(node) = stack.pop() {
        count += 1;
        if count > MAX_NODES {
            result.uncertain = true;
            result.packages_exact = false;
            break;
        }
        if node.kind() == "command" {
            let text = command_text(node, command, shell);
            let (words, uncertain) = literal_words(&text, shell);
            result.uncertain |= uncertain;
            result.packages_exact &= !uncertain;
            classify_words(&words, shell, depth, &mut result);
        }
        if matches!(
            node.kind(),
            "expansion" | "simple_expansion" | "variable" | "command_name_expr" | "ERROR"
        ) {
            result.uncertain = true;
            result.packages_exact = false;
        }
        // Every AST descendant is visited, including redirects, heredoc
        // expansions, loops, pipelines, script blocks and substitutions.
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    result
}
/// PowerShell redirections are syntax nodes, not process argv. Keep the
/// other elements in their original order and visit nested commands separately.
fn command_text(node: Node<'_>, source: &str, shell: Shell) -> String {
    if shell != Shell::Pwsh {
        return node
            .utf8_text(source.as_bytes())
            .unwrap_or_default()
            .to_owned();
    }
    let mut pieces = Vec::new();
    if let Some(name) = node.child_by_field_name("command_name") {
        pieces.push(name.utf8_text(source.as_bytes()).unwrap_or_default());
    }
    if let Some(elements) = node.child_by_field_name("command_elements") {
        let mut cursor = elements.walk();
        pieces.extend(
            elements
                .named_children(&mut cursor)
                .filter(|child| child.kind() != "redirection" && static_element(*child))
                .map(|child| child.utf8_text(source.as_bytes()).unwrap_or_default()),
        );
    }
    pieces.join(" ")
}
fn static_element(node: Node<'_>) -> bool {
    let mut stack = vec![node];
    let mut visited = 0;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > MAX_NODES
            || matches!(
                node.kind(),
                "variable" | "sub_expression" | "command_name_expr" | "ERROR"
            )
        {
            return false;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    true
}
/// Conservative CMD scanner, separate from Bash. Expansion, escaping, FOR/IF,
/// CALL and parenthesized syntax remain uncertain; no claimed safe prefix.
fn analyze_cmd(command: &str, depth: usize, result: &mut ShellAnalysis) {
    let mut quoted = false;
    let mut start = 0;
    for (index, ch) in command.char_indices() {
        if ch == '"' {
            quoted = !quoted;
        }
        if matches!(ch, '%' | '!' | '^' | '(' | ')') {
            result.uncertain = true;
            result.packages_exact = false;
        }
        if !quoted && matches!(ch, '&' | '|' | '\n' | '\r' | '(' | ')') {
            let (words, uncertain) = literal_words(&command[start..index], Shell::Cmd);
            result.uncertain |= uncertain;
            result.packages_exact &= !uncertain;
            classify_words(&words, Shell::Cmd, depth, result);
            start = index + ch.len_utf8();
        }
    }
    let (words, uncertain) = literal_words(&command[start..], Shell::Cmd);
    result.uncertain |= uncertain || quoted;
    result.packages_exact &= !(uncertain || quoted);
    classify_words(&words, Shell::Cmd, depth, result);
}
/// Literal argv extraction only. It intentionally refuses interpolation rather
/// than interpreting environment values, substitutions or expression syntax.
fn literal_words(text: &str, shell: Shell) -> (Vec<String>, bool) {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    let mut uncertain = shell == Shell::Pwsh && text.contains("''");
    for ch in text.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                word.push(ch);
            }
            if (q == '"' && matches!(ch, '$' | '`' | '%' | '!')) || ch == '\\' {
                uncertain = true;
            }
            continue;
        }
        if ch == '"' || (ch == '\'' && shell != Shell::Cmd) {
            quote = Some(ch);
            started = true;
        } else if ch.is_whitespace() {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
        } else if matches!(ch, ';' | '&' | '|' | '<' | '>') {
            uncertain = true;
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            if matches!(ch, '$' | '`' | '^' | '%' | '!' | '(' | ')' | '{' | '}')
                || (ch == '\\' && shell == Shell::Sh)
            {
                uncertain = true;
            }
            word.push(ch);
            started = true;
        }
    }
    if started {
        words.push(word);
    }
    (words, uncertain || quote.is_some())
}
fn basename(word: &str) -> String {
    word.rsplit(['/', '\\'])
        .next()
        .unwrap_or(word)
        .to_ascii_lowercase()
        .trim_end_matches(".exe")
        .to_owned()
}
fn assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().enumerate().all(|(index, ch)| {
                ch == '_' || ch.is_ascii_alphabetic() || (index > 0 && ch.is_ascii_digit())
            })
    })
}
fn classify_words(words: &[String], shell: Shell, depth: usize, result: &mut ShellAnalysis) {
    if depth > MAX_DEPTH {
        result.uncertain = true;
        result.packages_exact = false;
        return;
    }
    let words = &words[words.iter().take_while(|word| assignment(word)).count()..];
    let Some(first) = words.first() else {
        return;
    };
    let name = basename(first);
    let args = &words[1..];
    if classify_wrapper(&name, args, shell, depth, result) {
        return;
    }
    let lower: Vec<_> = args.iter().map(|arg| arg.to_ascii_lowercase()).collect();
    if matches!(name.as_str(), "npm" | "pnpm" | "yarn" | "bun")
        && lower
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "run" | "exec" | "dlx" | "x" | "test"))
    {
        result.uncertain = true;
    }
    let has = |value: &str| lower.iter().any(|arg| arg == value);
    if name == "git" {
        classify_git(args, result);
    }
    let publish = matches!(name.as_str(), "npm" | "pnpm" | "yarn" | "bun" | "cargo")
        && lower.first().is_some_and(|arg| arg == "publish");
    let remote = (name == "gh"
        && lower
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "pr" | "release")))
        || (name == "curl"
            && (args.iter().any(|arg| arg.starts_with("-T"))
                || lower
                    .iter()
                    .any(|arg| arg.starts_with("--upload") || arg.starts_with("--data"))
                || has("-d")
                || has("-x")))
        || (name == "wget" && lower.iter().any(|arg| arg.starts_with("--post")))
        || matches!(name.as_str(), "scp" | "sftp" | "rsync");
    if publish || remote {
        result.category = result.category.max(ShellCategory::PublishOrRemote);
    }
    if matches!(
        name.as_str(),
        "rm" | "del" | "erase" | "rmdir" | "rd" | "remove-item" | "ri"
    ) && mass_delete(args, shell)
    {
        result.category = ShellCategory::MassDelete;
    }
    if let Some(index) = install_subcommand(&name, &lower) {
        result.category = result.category.max(ShellCategory::DependencyInstall);
        extract_packages(&args[index + 1..], result);
    } else if name == "yarn" && args.is_empty() {
        result.category = result.category.max(ShellCategory::DependencyInstall);
        result.packages_exact = false;
    }
}
#[allow(clippy::too_many_lines)] // One bounded dispatch table for known shell/interpreter wrappers.
fn classify_wrapper(
    name: &str,
    args: &[String],
    shell: Shell,
    depth: usize,
    result: &mut ShellAnalysis,
) -> bool {
    let lower: Vec<_> = args.iter().map(|arg| arg.to_ascii_lowercase()).collect();
    if matches!(
        name,
        "sh" | "bash" | "dash" | "zsh" | "cmd" | "powershell" | "pwsh"
    ) {
        let nested = match name {
            "cmd" => Shell::Cmd,
            "powershell" | "pwsh" => Shell::Pwsh,
            _ => Shell::Sh,
        };
        if let Some(index) = lower
            .iter()
            .position(|arg| matches!(arg.as_str(), "-c" | "/c" | "-command"))
        {
            if let Some(body) = args.get(index + 1) {
                result.merge(analyze_inner(body, nested, depth + 1));
            } else {
                result.uncertain = true;
            }
        } else {
            result.uncertain = true;
            result.packages_exact = false;
        }
        return true;
    }
    if matches!(
        name,
        "env" | "command" | "builtin" | "exec" | "sudo" | "nohup" | "time" | "xargs"
    ) {
        result.uncertain |= matches!(name, "sudo" | "xargs");
        if name == "command" && lower.iter().any(|arg| matches!(arg.as_str(), "-v" | "-V")) {
            return true;
        }
        let mut index = 0;
        while let Some(arg) = args.get(index) {
            if assignment(arg) || arg == "--" {
                index += 1;
                continue;
            }
            if arg.starts_with('-') {
                if matches!(arg.as_str(), "-u" | "--unset" | "--user" | "-g" | "--group") {
                    index += 2;
                } else if matches!(arg.as_str(), "-i" | "--ignore-environment" | "-p" | "-n") {
                    index += 1;
                } else {
                    result.uncertain = true;
                    return true;
                }
            } else {
                break;
            }
        }
        classify_words(
            args.get(index..).unwrap_or_default(),
            shell,
            depth + 1,
            result,
        );
        return true;
    }
    if name == "find" {
        if let Some(index) = lower
            .iter()
            .position(|arg| matches!(arg.as_str(), "-exec" | "-execdir"))
        {
            result.uncertain = true;
            classify_words(&args[index + 1..], shell, depth + 1, result);
        }
    }
    if shell == Shell::Cmd && matches!(name, "for" | "if" | "do" | "else" | "call") {
        result.uncertain = true;
        let index = if name == "call" || name == "do" || name == "else" {
            Some(0)
        } else {
            lower
                .iter()
                .position(|arg| arg == "do" || arg == "else")
                .map(|index| index + 1)
        };
        if let Some(index) = index {
            classify_words(&args[index..], shell, depth + 1, result);
        }
        return true;
    }
    if matches!(
        name,
        "eval"
            | "invoke-expression"
            | "iex"
            | "python"
            | "python3"
            | "node"
            | "perl"
            | "ruby"
            | "source"
            | "."
            | "start-process"
            | "invoke-command"
            | "npx"
    ) {
        result.uncertain = true;
        result.packages_exact = false;
        if matches!(name, "python" | "python3")
            && lower.first().is_some_and(|arg| arg == "-m")
            && lower
                .get(1)
                .is_some_and(|arg| matches!(arg.as_str(), "pip" | "pip3"))
        {
            classify_words(&args[1..], shell, depth + 1, result);
        }
    }
    false
}
fn short_flag(args: &[String], flag: char) -> bool {
    args.iter().any(|arg| {
        arg.starts_with('-') && !arg.starts_with("--") && arg.chars().skip(1).any(|ch| ch == flag)
    })
}
fn mass_delete(args: &[String], shell: Shell) -> bool {
    let recursive = short_flag(args, 'r')
        || short_flag(args, 'R')
        || args.iter().any(|arg| {
            arg.eq_ignore_ascii_case("/s")
                || arg.eq_ignore_ascii_case("-recurse")
                || arg == "--recursive"
        });
    let targets = args
        .iter()
        .filter(|arg| !(arg.starts_with('-') || shell == Shell::Cmd && arg.starts_with('/')))
        .map(|arg| {
            if shell == Shell::Pwsh {
                arg.split(',').count()
            } else {
                1
            }
        })
        .sum::<usize>();
    recursive
        || targets > 3
        || args
            .iter()
            .any(|arg| arg.contains('*') || arg.contains('?'))
}
fn classify_git(args: &[String], result: &mut ShellAnalysis) {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if matches!(arg.as_str(), "-C" | "-c" | "--git-dir" | "--work-tree") {
            index += 2;
        } else if arg.starts_with('-') {
            index += 1;
        } else {
            break;
        }
    }
    let Some(subcommand) = args.get(index) else {
        return;
    };
    let rest = &args[index + 1..];
    let has = |value: &str| rest.iter().any(|arg| arg == value);
    let force = short_flag(rest, 'f') || rest.iter().any(|arg| arg.starts_with("--force"));
    let destructive = match subcommand.as_str() {
        "reset" => has("--hard"),
        "clean" | "push" => force,
        "checkout" => has("--") && has("."),
        "branch" => has("-D"),
        _ => false,
    };
    if destructive {
        result.category = result.category.max(ShellCategory::DestructiveGit);
    } else if subcommand == "push" {
        result.category = result.category.max(ShellCategory::PublishOrRemote);
    }
    if subcommand == "rm" && mass_delete(rest, Shell::Sh) {
        result.category = ShellCategory::MassDelete;
    }
    if args.iter().any(|arg| arg.starts_with("alias.")) {
        result.uncertain = true;
    }
}
fn install_subcommand(name: &str, args: &[String]) -> Option<usize> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if matches!(
            arg.as_str(),
            "--prefix"
                | "--cwd"
                | "--directory"
                | "--manifest-path"
                | "--registry"
                | "--cache"
                | "--python"
        ) {
            index += 2;
        } else if matches!(arg.as_str(), "-g" | "--global")
            || (name == "cargo" && arg.starts_with('+'))
        {
            index += 1;
        } else {
            break;
        }
    }
    let first = args.get(index)?.as_str();
    match name {
        "uv" if first == "pip" && args.get(index + 1).is_some_and(|arg| arg == "install") => {
            Some(index + 1)
        }
        "go" if first == "get" => Some(index),
        "npm" | "pnpm" | "yarn" | "bun" | "cargo" | "pip" | "pip3" | "uv" | "poetry" | "winget"
        | "choco" | "brew" | "apt" | "apt-get"
            if matches!(first, "install" | "add" | "i" | "ci") =>
        {
            Some(index)
        }
        _ => None,
    }
}
#[allow(clippy::too_many_lines)] // Keep the bounded literal package-option table in one reviewable function.
fn extract_packages(args: &[String], result: &mut ShellAnalysis) {
    if args.is_empty() {
        result.packages_exact = false;
    }
    let mut index = 0;
    while let Some(package) = args.get(index) {
        index += 1;
        if package == "--" {
            continue;
        }
        if package.starts_with('-') {
            let option = package.split('=').next().unwrap_or(package);
            if matches!(
                option,
                "--save"
                    | "--save-dev"
                    | "--save-exact"
                    | "--no-save"
                    | "--ignore-scripts"
                    | "--global"
                    | "-g"
                    | "-D"
                    | "-S"
                    | "--dev"
                    | "-y"
                    | "--yes"
                    | "--cask"
                    | "--exact"
                    | "--upgrade"
                    | "-U"
            ) {
                continue;
            }
            if matches!(option, "--id" | "--name") {
                if let Some(value) = args.get(index) {
                    result.packages.push(value.clone());
                    index += 1;
                } else {
                    result.packages_exact = false;
                }
                result.packages_exact &= option == "--id";
                continue;
            }
            if matches!(
                option,
                "--registry"
                    | "--prefix"
                    | "--workspace"
                    | "-w"
                    | "--cache"
                    | "--target"
                    | "--version"
                    | "--index-url"
                    | "--extra-index-url"
                    | "--manifest-path"
                    | "--git"
                    | "--branch"
                    | "--source"
            ) {
                if !package.contains('=') {
                    index += 1;
                }
                continue;
            }
            result.packages_exact = false;
            if !package.contains('=')
                && matches!(
                    option,
                    "-r" | "--requirement" | "-c" | "--constraint" | "-e" | "--editable" | "--path"
                )
            {
                index += 1;
            }
            continue;
        }
        if package.chars().any(|ch| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '$' | '`'
                        | '%'
                        | '!'
                        | '('
                        | ')'
                        | ';'
                        | '|'
                        | '&'
                        | '<'
                        | '>'
                        | '*'
                        | '?'
                        | '{'
                        | '}'
                )
        }) || matches!(package.as_str(), "." | "..")
        {
            result.packages_exact = false;
        } else {
            result.packages.push(package.clone());
        }
    }
    if result.packages.is_empty() {
        result.packages_exact = false;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grammar_abi_and_positive_classification() {
        for shell in [Shell::Sh, Shell::Pwsh] {
            let mut parser = Parser::new();
            let language = if shell == Shell::Sh {
                tree_sitter_bash::LANGUAGE.into()
            } else {
                tree_sitter_powershell::LANGUAGE.into()
            };
            parser
                .set_language(&language)
                .expect("approved grammar ABI must initialize");
            let result = analyze("npm install exact-package@1.2.3", shell);
            assert_eq!(result.category, ShellCategory::DependencyInstall);
            assert_eq!(result.packages, ["exact-package@1.2.3"]);
            assert!(!result.uncertain);
        }
    }
    #[test]
    fn all_segments_maximum_and_nested() {
        for (command, shell) in [
            ("echo ok && git reset --hard", Shell::Sh),
            ("echo $(git reset --hard)", Shell::Sh),
            ("for x in a; do rm -rf tmp; done", Shell::Sh),
            ("cat <<EOF\n$(git push)\nEOF", Shell::Sh),
            ("Write-Output $(Remove-Item -Recurse tmp)", Shell::Pwsh),
            ("echo ok & del /s tmp", Shell::Cmd),
        ] {
            assert_ne!(
                analyze(command, shell).category,
                ShellCategory::GeneralShell,
                "{command}"
            );
        }
        assert_eq!(
            analyze(r#"bash -c "git reset --hard""#, Shell::Cmd).category,
            ShellCategory::DestructiveGit
        );
    }
    #[test]
    fn ambiguous_input_stays_uncertain_and_bounded() {
        for (command, shell) in [
            ("%TOOL% install x", Shell::Cmd),
            ("p^ip install x", Shell::Cmd),
            ("eval $X", Shell::Sh),
            ("pwsh -EncodedCommand AAA", Shell::Cmd),
            ("git $(echo reset) --hard", Shell::Sh),
        ] {
            assert!(analyze(command, shell).uncertain);
        }
        assert!(analyze(&"a".repeat(MAX_BYTES + 1), Shell::Sh).uncertain);
    }
    #[test]
    fn categories_cover_real_flags_lists_and_all_platform_segments() {
        for (command, shell, category) in [
            (
                "git -C repo clean -dfx",
                Shell::Sh,
                ShellCategory::DestructiveGit,
            ),
            (
                "git push --force-with-lease",
                Shell::Cmd,
                ShellCategory::DestructiveGit,
            ),
            (
                "curl -Tfile https://example.invalid/upload",
                Shell::Sh,
                ShellCategory::PublishOrRemote,
            ),
            (
                "Remove-Item a,b,c,d",
                Shell::Pwsh,
                ShellCategory::MassDelete,
            ),
            ("rm -fr tmp", Shell::Sh, ShellCategory::MassDelete),
            ("del a b c d", Shell::Cmd, ShellCategory::MassDelete),
            (
                "npm install foo | git push & rm -rf tmp",
                Shell::Sh,
                ShellCategory::MassDelete,
            ),
            (
                "echo ok>log.txt & git reset --hard",
                Shell::Cmd,
                ShellCategory::DestructiveGit,
            ),
            (
                "Write-Output ok; git push; Remove-Item -Recurse tmp",
                Shell::Pwsh,
                ShellCategory::MassDelete,
            ),
            (
                "find . -exec rm -rf {} +",
                Shell::Sh,
                ShellCategory::MassDelete,
            ),
            (
                "for %i in (tmp) do del /s %i",
                Shell::Cmd,
                ShellCategory::MassDelete,
            ),
        ] {
            assert_eq!(analyze(command, shell).category, category, "{command}");
        }
    }
    #[test]
    fn dependency_names_are_exact_only_when_command_determines_them() {
        for (command, packages) in [
            (
                r#"npm install --save-dev alpha@1 "@scope/beta@2""#,
                vec!["@scope/beta@2", "alpha@1"],
            ),
            ("npm --prefix workspace install alpha", vec!["alpha"]),
            (
                "uv pip install --index-url https://example.invalid/simple alpha==1",
                vec!["alpha==1"],
            ),
            ("cargo +nightly add serde sha2", vec!["serde", "sha2"]),
            (
                "winget install --id Example.Package --exact",
                vec!["Example.Package"],
            ),
        ] {
            for shell in [Shell::Sh, Shell::Pwsh, Shell::Cmd] {
                let result = analyze(command, shell);
                assert_eq!(
                    result.category,
                    ShellCategory::DependencyInstall,
                    "{command}"
                );
                assert_eq!(result.packages, packages, "{command}");
                assert!(result.packages_exact, "{command}");
            }
        }
        for command in [
            "npm install",
            "pip install -r requirements.txt",
            "cargo install --path .",
        ] {
            let result = analyze(command, Shell::Sh);
            assert_eq!(result.category, ShellCategory::DependencyInstall);
            assert!(!result.packages_exact);
        }
        assert_eq!(
            analyze("npm run install", Shell::Sh).category,
            ShellCategory::GeneralShell
        );
        assert!(analyze("npm run install", Shell::Sh).uncertain);
    }
    #[test]
    fn opaque_interpreters_and_depth_exhaustion_never_claim_verified_analysis() {
        for (command, shell) in [
            ("node -e 'require(\"fs\").rmSync(\"x\")'", Shell::Sh),
            ("pwsh -EncodedCommand AAA", Shell::Cmd),
            ("env -S 'rm -rf tmp'", Shell::Sh),
        ] {
            assert!(analyze(command, shell).uncertain);
        }
        assert!(analyze_inner("npm install foo", Shell::Sh, MAX_DEPTH + 1).uncertain);
    }
    #[test]
    fn valid_native_powershell_scoped_package_remains_visible_with_grammar_uncertainty() {
        // Native PowerShell accepts this literal argv. The pinned grammar may
        // model the @ token as a variable; that is analysis uncertainty, never
        // evidence that the command is invalid or permission to bypass consent.
        let result = analyze("npm install @scope/pkg", Shell::Pwsh);
        assert_eq!(result.category, ShellCategory::DependencyInstall);
        assert!(
            result.packages.is_empty(),
            "do not present a grammar-truncated package name as literal argv"
        );
        assert!(result.uncertain);
        assert!(!result.packages_exact);
    }
}
