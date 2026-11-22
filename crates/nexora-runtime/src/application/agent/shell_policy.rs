//! Bounded native shell analysis. Classification never authorizes execution.
use serde::Serialize;
use tree_sitter::{Node, Parser};
const MAX_BYTES: usize = 32 * 1024;
const MAX_NODES: usize = 16_384;
const MAX_DEPTH: usize = 16;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum ShellCategory {
    GeneralShell,
    DependencyInstall,
    PublishOrRemote,
    DestructiveGit,
    MassDelete,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShellAnalysis {
    pub category: ShellCategory,
    pub packages: Vec<String>,
    pub packages_exact: bool,
    pub uncertain: bool,
    pub sandbox_posture: &'static str,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub long_running: bool,
    #[serde(skip)]
    pub long_running_pattern: Option<&'static str>,
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
            long_running: false,
            long_running_pattern: None,
        }
    }
    fn merge(&mut self, other: Self) {
        self.category = self.category.max(other.category);
        self.uncertain |= other.uncertain;
        self.packages_exact &= other.packages_exact;
        self.packages.extend(other.packages);
        self.long_running |= other.long_running;
        if self.long_running_pattern.is_none() {
            self.long_running_pattern = other.long_running_pattern;
        }
    }
}
pub(crate) fn analyze(command: &str, shell: Shell) -> ShellAnalysis {
    let mut result = analyze_inner(command, shell, 0);
    result.packages.sort();
    result.packages.dedup();
    result
}
/// Returns the matched long-running dev-server/watch pattern if `command` would
/// start an interactive dev server or unbounded watch loop on the current platform shell.
pub(crate) fn blocked_long_running_command(command: &str) -> Option<&'static str> {
    blocked_long_running_command_for(command, Shell::platform())
}
pub(crate) fn blocked_long_running_command_for(
    command: &str,
    shell: Shell,
) -> Option<&'static str> {
    let analysis = analyze(command, shell);
    if analysis.long_running {
        analysis
            .long_running_pattern
            .or(Some("long-running dev server or watch command"))
    } else {
        None
    }
}
/// Fixed-vocabulary denial message for blocked dev-server / watch commands (`S11-02`).
pub(crate) fn long_running_denial_message(pattern: &str) -> String {
    format!(
        "Error: interactive dev-server or watch command ({pattern}) is blocked in execute_command \
         because it runs indefinitely; use a finite one-shot command instead \
         (for example 'npx vitest run', 'npm run build', or 'cargo test')"
    )
}
/// Model-supplied posture, escalation flags and unknown execution options never authorize automation.
pub(crate) fn automatic_general_shell(arguments: &str) -> bool {
    automatic_general_shell_for(arguments, Shell::platform())
}

fn automatic_general_shell_for(arguments: &str, shell: Shell) -> bool {
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return false;
    };
    let Some(object) = args.as_object() else {
        return false;
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "command" | "cwd"))
    {
        return false;
    }
    let Some(command) = args.get("command").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let analysis = analyze(command, shell);
    analysis.category == ShellCategory::GeneralShell
        && !analysis.uncertain
        && !analysis.long_running
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
            let (text, omitted_dynamic) = command_text(node, command, shell);
            let (words, uncertain) = literal_words(&text, shell);
            let uncertain = uncertain || omitted_dynamic;
            result.uncertain |= uncertain;
            result.packages_exact &= !uncertain;
            classify_words(&words, shell, depth, &mut result);
        }
        if matches!(
            node.kind(),
            "expansion"
                | "simple_expansion"
                | "command_substitution"
                | "process_substitution"
                | "arithmetic_expansion"
                | "function_definition"
                | "variable"
                | "sub_expression"
                | "command_name_expr"
                | "redirected_statement"
                | "file_redirect"
                | "heredoc_redirect"
                | "herestring_redirect"
                | "redirection"
                | "ERROR"
        ) {
            result.uncertain = true;
            result.packages_exact = false;
        }
        if shell == Shell::Sh {
            let mut token_cursor = node.walk();
            for child in node.children(&mut token_cursor) {
                if !child.is_named() && child.kind() == "&" {
                    result.uncertain = true;
                    result.packages_exact = false;
                }
            }
        }
        // Every AST descendant is visited, including redirects, heredoc
        // expansions, loops, pipelines, script blocks and substitutions.
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    result
}
/// PowerShell redirections are syntax nodes, not process argv. Keep the
/// static elements in their original order and visit nested commands separately.
/// Omitting a dynamic argv element must invalidate certainty even when the
/// remaining text looks like a complete literal command.
fn command_text(node: Node<'_>, source: &str, shell: Shell) -> (String, bool) {
    if shell != Shell::Pwsh {
        return (
            node.utf8_text(source.as_bytes())
                .unwrap_or_default()
                .to_owned(),
            false,
        );
    }
    let mut pieces = Vec::new();
    let mut omitted_dynamic = false;
    if let Some(name) = node.child_by_field_name("command_name") {
        pieces.push(name.utf8_text(source.as_bytes()).unwrap_or_default());
    }
    if let Some(elements) = node.child_by_field_name("command_elements") {
        let mut cursor = elements.walk();
        for child in elements.named_children(&mut cursor) {
            if child.kind() == "redirection" {
                continue;
            }
            if static_element(child) {
                pieces.push(child.utf8_text(source.as_bytes()).unwrap_or_default());
            } else {
                omitted_dynamic = true;
            }
        }
    }
    (pieces.join(" "), omitted_dynamic)
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
    if matches!(
        name.as_str(),
        "sudo" | "su" | "doas" | "pkexec" | "runas" | "nsenter" | "unshare"
    ) || (name == "start-process" && args.iter().any(|arg| arg.eq_ignore_ascii_case("runas")))
    {
        result.uncertain = true;
    }
    if classify_wrapper(&name, args, shell, depth, result) {
        return;
    }
    let lower: Vec<_> = args.iter().map(|arg| arg.to_ascii_lowercase()).collect();
    if let Some(pattern) = detect_long_running(&name, args, &lower, depth) {
        result.long_running = true;
        if result.long_running_pattern.is_none() {
            result.long_running_pattern = Some(pattern);
        }
        result.uncertain = true;
    }
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
        if let Some(index) = lower.iter().position(|arg| match nested {
            Shell::Cmd => matches!(arg.as_str(), "/c" | "/k"),
            Shell::Pwsh => matches!(arg.as_str(), "-c" | "-command"),
            Shell::Sh => {
                arg == "-c"
                    || (arg.starts_with('-')
                        && !arg.starts_with("--")
                        && arg.chars().skip(1).any(|ch| ch == 'c'))
            }
        }) {
            if nested == Shell::Cmd && lower[index] == "/k" {
                result.uncertain = true;
                result.packages_exact = false;
            }
            if let Some(body) = args.get(index + 1) {
                let tail = &args[index + 1..];
                if nested == Shell::Sh || tail.len() == 1 {
                    // POSIX -c executes one argv element; later argv are positional.
                    result.merge(analyze_inner(body, nested, depth + 1));
                } else {
                    // CMD /C and PowerShell -Command consume the remaining tail.
                    // Literal bare words reconstruct exactly. Quotes/expansions
                    // can lose syntax in outer tokenization: still inspect the
                    // whole tail, but never claim certain analysis/packages.
                    let exact = tail.iter().all(|word| {
                        !word.is_empty()
                            && word.chars().all(|ch| {
                                ch.is_ascii_alphanumeric()
                                    || matches!(
                                        ch,
                                        '_' | '-' | '.' | '/' | '\\' | ':' | '@' | '+' | '='
                                    )
                            })
                    });
                    result.merge(analyze_inner(&tail.join(" "), nested, depth + 1));
                    result.uncertain |= !exact;
                    result.packages_exact &= exact;
                }
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
    if (shell == Shell::Cmd && name == "start")
        || matches!(
            name,
            "eval"
                | "invoke-expression"
                | "iex"
                | "python"
                | "python3"
                | "node"
                | "deno"
                | "tsx"
                | "ts-node"
                | "perl"
                | "ruby"
                | "source"
                | "."
                | "start-process"
                | "invoke-command"
                | "npx"
        )
    {
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

fn has_info_flag(lower: &[String]) -> bool {
    lower
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help" | "-v" | "-V" | "--version"))
}

fn is_dev_or_watch_script_name(script: &str) -> bool {
    matches!(
        script,
        "dev" | "start" | "serve" | "watch" | "preview" | "storybook"
    ) || script.starts_with("dev:")
        || script.starts_with("watch:")
        || script.starts_with("serve:")
        || script.ends_with(":dev")
        || script.ends_with(":watch")
        || script.ends_with(":serve")
}

/// Detects known interactive dev servers and unbounded watch loops (`S11-02`).
/// Operates strictly on parsed command argv (never raw substring matching) so
/// benign commands like `echo "npm run dev"` or `vitest run` are never falsely blocked.
#[allow(clippy::too_many_lines)] // Bounded argv dispatch table for dev-server and watch commands.
fn detect_long_running(
    name: &str,
    args: &[String],
    lower: &[String],
    depth: usize,
) -> Option<&'static str> {
    if depth > MAX_DEPTH || has_info_flag(lower) {
        return None;
    }
    match name {
        "npx" => {
            let mut idx = 0;
            while let Some(arg) = lower.get(idx) {
                if matches!(arg.as_str(), "-p" | "--package" | "-c" | "--call") {
                    idx += 2;
                } else if arg.starts_with('-') {
                    idx += 1;
                } else {
                    break;
                }
            }
            let tool = args.get(idx)?;
            let inner_name = basename(tool);
            let inner_args = &args[idx + 1..];
            let inner_lower = &lower[idx + 1..];
            if inner_name == "serve" {
                return Some("npx serve");
            }
            detect_long_running(&inner_name, inner_args, inner_lower, depth + 1)
        }
        "npm" | "pnpm" | "yarn" | "bun" => {
            if name == "bun"
                && lower
                    .iter()
                    .any(|arg| matches!(arg.as_str(), "--watch" | "--hot"))
            {
                return Some("bun --watch");
            }
            let mut idx = 0;
            while let Some(arg) = lower.get(idx) {
                if matches!(
                    arg.as_str(),
                    "--prefix" | "--cwd" | "-c" | "--dir" | "--filter" | "--workspace" | "-w"
                ) {
                    idx += 2;
                } else if arg.starts_with('-') {
                    idx += 1;
                } else {
                    break;
                }
            }
            let sub = lower.get(idx)?.as_str();
            let rest_args = &args[idx + 1..];
            let rest_lower = &lower[idx + 1..];
            match sub {
                "start" => Some("package manager start"),
                "dev" | "serve" | "watch" | "preview" => Some("package manager dev/watch"),
                "tauri" if rest_lower.iter().any(|a| a == "dev") => Some("tauri dev"),
                "run" => {
                    let script_idx = rest_lower.iter().position(|a| !a.starts_with('-'))?;
                    let script = rest_lower[script_idx].as_str();
                    if is_dev_or_watch_script_name(script) {
                        return Some("npm/pnpm/yarn/bun dev/watch script");
                    }
                    if script == "tauri" && rest_lower[script_idx + 1..].iter().any(|a| a == "dev")
                    {
                        return Some("tauri dev");
                    }
                    None
                }
                "exec" | "dlx" | "x" => {
                    let tool_idx = rest_lower.iter().position(|a| !a.starts_with('-'))?;
                    let inner_name = basename(&rest_args[tool_idx]);
                    detect_long_running(
                        &inner_name,
                        &rest_args[tool_idx + 1..],
                        &rest_lower[tool_idx + 1..],
                        depth + 1,
                    )
                }
                _ => None,
            }
        }
        "cargo" => {
            let mut idx = 0;
            while let Some(arg) = lower.get(idx) {
                if arg.starts_with('+') {
                    idx += 1;
                } else if matches!(arg.as_str(), "-c" | "--manifest-path" | "--config") {
                    idx += 2;
                } else if arg.starts_with('-') {
                    idx += 1;
                } else {
                    break;
                }
            }
            let sub = lower.get(idx)?.as_str();
            let rest = &lower[idx + 1..];
            match sub {
                "watch" => Some("cargo watch"),
                "bacon" => Some("cargo bacon"),
                "tauri" if rest.iter().any(|a| a == "dev") => Some("cargo tauri dev"),
                "leptos" if rest.iter().any(|a| a == "watch") => Some("cargo leptos watch"),
                _ => None,
            }
        }
        "tauri" | "cargo-tauri" => lower
            .iter()
            .find(|a| !a.starts_with('-'))
            .is_some_and(|a| a == "dev")
            .then_some("tauri dev"),
        "vite" => {
            let first = lower
                .iter()
                .find(|a| !a.starts_with('-'))
                .map(String::as_str);
            match first {
                None | Some("dev" | "serve" | "preview") => Some("vite dev server"),
                _ => None,
            }
        }
        "vitest" => {
            let has_explicit_watch = lower.iter().any(|a| {
                matches!(a.as_str(), "watch" | "--watch" | "-w")
                    && !lower
                        .iter()
                        .any(|b| matches!(b.as_str(), "--watch=false" | "--no-watch"))
            });
            let is_finite = lower.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "run"
                        | "--run"
                        | "bench"
                        | "list"
                        | "related"
                        | "typecheck"
                        | "--watch=false"
                        | "--no-watch"
                )
            });
            (has_explicit_watch || !is_finite).then_some("vitest watch")
        }
        "jest" => lower
            .iter()
            .any(|a| matches!(a.as_str(), "--watch" | "--watchall"))
            .then_some("jest --watch"),
        "tsc" => lower
            .iter()
            .any(|a| matches!(a.as_str(), "-w" | "--watch"))
            .then_some("tsc --watch"),
        "next" | "nuxt" | "astro" | "remix" | "svelte-kit" | "expo" => lower
            .iter()
            .find(|a| !a.starts_with('-'))
            .is_some_and(|a| matches!(a.as_str(), "dev" | "start" | "serve"))
            .then_some("frontend dev server"),
        "webpack" => lower
            .iter()
            .any(|a| matches!(a.as_str(), "serve" | "--watch" | "-w"))
            .then_some("webpack serve/watch"),
        "webpack-dev-server" => Some("webpack-dev-server"),
        "esbuild" => lower
            .iter()
            .any(|a| {
                a == "--watch"
                    || a.starts_with("--watch=")
                    || a == "--serve"
                    || a.starts_with("--serve=")
            })
            .then_some("esbuild serve/watch"),
        "trunk" => lower
            .iter()
            .find(|a| !a.starts_with('-'))
            .is_some_and(|a| matches!(a.as_str(), "serve" | "watch"))
            .then_some("trunk serve/watch"),
        "bacon" => Some("bacon watch"),
        "nodemon" | "ts-node-dev" | "live-server" | "http-server" | "browser-sync" => {
            Some("dev server / watch")
        }
        "storybook" => lower
            .iter()
            .find(|a| !a.starts_with('-'))
            .is_none_or(|a| a == "dev")
            .then_some("storybook dev"),
        "tsx" => lower
            .first()
            .is_some_and(|a| a == "watch")
            .then_some("tsx watch"),
        "node" | "deno" => lower
            .iter()
            .any(|a| a == "--watch" || a.starts_with("--watch-path"))
            .then_some("node/deno --watch"),
        "python" | "python3" => {
            if let Some(m_idx) = lower.iter().position(|a| a == "-m") {
                if let Some(module) = lower.get(m_idx + 1).map(String::as_str) {
                    if matches!(
                        module,
                        "http.server" | "uvicorn" | "gunicorn" | "livereload" | "watchfiles"
                    ) || (module == "flask" && lower[m_idx + 2..].iter().any(|a| a == "run"))
                    {
                        return Some("python dev server");
                    }
                }
            }
            if lower
                .windows(2)
                .any(|pair| pair[0].ends_with("manage.py") && pair[1] == "runserver")
            {
                return Some("django runserver");
            }
            None
        }
        "uvicorn" | "gunicorn" => Some("python server"),
        "flask" => lower.iter().any(|a| a == "run").then_some("flask run"),
        "watch" => Some("watch loop"),
        "tail" => (short_flag(args, 'f')
            || short_flag(args, 'F')
            || lower
                .iter()
                .any(|a| a == "--follow" || a.starts_with("--follow=")))
        .then_some("tail --follow"),
        "docker" => {
            let detached = short_flag(args, 'd') || lower.iter().any(|a| a == "--detach");
            let is_compose_up = lower.windows(2).any(|w| w == ["compose", "up"]);
            (is_compose_up && !detached).then_some("docker compose up")
        }
        "docker-compose" => {
            let detached = short_flag(args, 'd') || lower.iter().any(|a| a == "--detach");
            let is_up = lower.iter().any(|a| a == "up");
            (is_up && !detached).then_some("docker-compose up")
        }
        _ => None,
    }
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
    fn windows_interpreter_wrappers_classify_the_complete_unquoted_tail() {
        for (command, category) in [
            ("cmd /C git reset --hard", ShellCategory::DestructiveGit),
            (
                "powershell -Command Remove-Item -Recurse tmp",
                ShellCategory::MassDelete,
            ),
            (
                "pwsh -c Remove-Item -Recurse tmp",
                ShellCategory::MassDelete,
            ),
            (
                "cmd /C cmd /C git reset --hard",
                ShellCategory::DestructiveGit,
            ),
        ] {
            let result = analyze(command, Shell::Cmd);
            assert_eq!(result.category, category, "{command}");
            assert!(!result.uncertain, "{command}");
        }
        let result = analyze("cmd /C npm install alpha@1 @scope/beta@2", Shell::Cmd);
        assert_eq!(result.category, ShellCategory::DependencyInstall);
        assert_eq!(result.packages, ["@scope/beta@2", "alpha@1"]);
        assert!(result.packages_exact);
        assert!(!result.uncertain);
        // Lost quoting/expansion boundaries cannot claim exact reconstruction.
        let result = analyze(r#"cmd /C "npm install alpha" "%PACKAGE%""#, Shell::Cmd);
        assert!(result.uncertain);
        assert!(!result.packages_exact);
        // POSIX -c's additional argv do not become executable command text.
        assert_eq!(
            analyze(r#"sh -c "echo ok" git reset --hard"#, Shell::Cmd).category,
            ShellCategory::GeneralShell
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
    fn unresolved_powershell_arguments_never_claim_complete_literal_argv() {
        for command in [
            "npm install alpha $(Write-Output beta)",
            "npm install alpha $packages",
            r#"npm install alpha "$(Write-Output beta)""#,
            "npm install alpha prefix$(Write-Output beta)",
            r#"git reset $(Write-Output "--hard")"#,
            "git reset $flags",
            r#"git reset "$(Write-Output '--hard')""#,
        ] {
            let result = analyze(command, Shell::Pwsh);
            assert!(result.uncertain, "{command}: {result:?}");
            assert!(!result.packages_exact, "{command}: {result:?}");
            assert!(
                !result.packages.iter().any(|package| package == "beta"),
                "unresolved expression output must not be invented"
            );
        }
    }
    #[test]
    fn selected_shell_nested_powershell_propagates_dynamic_argument_uncertainty() {
        for (command, shell) in [
            (
                "pwsh -Command 'npm install alpha $(Write-Output beta)'",
                Shell::Sh,
            ),
            (
                r#"pwsh -Command 'git reset $(Write-Output "--hard")'"#,
                Shell::Sh,
            ),
            (
                "pwsh -Command 'npm install alpha $(Write-Output beta)'",
                Shell::Pwsh,
            ),
            (
                r#"pwsh -Command "npm install alpha $(Write-Output beta)""#,
                Shell::Cmd,
            ),
            (
                r#"powershell -Command "git reset $(Write-Output '--hard')""#,
                Shell::Cmd,
            ),
        ] {
            let result = analyze(command, shell);
            assert!(result.uncertain, "{shell:?}: {command}: {result:?}");
            assert!(!result.packages_exact, "{shell:?}: {command}: {result:?}");
            if command.contains("npm install") {
                assert_eq!(result.category, ShellCategory::DependencyInstall);
                assert_eq!(result.packages, ["alpha"]);
            }
            let metadata = serde_json::to_value(&result).unwrap();
            assert_eq!(metadata["uncertain"], true);
            assert_eq!(metadata["packages_exact"], false);
            assert_eq!(metadata["sandbox_posture"], "PerCommandConsent");
        }
    }
    #[test]
    fn powershell_dynamic_arguments_preserve_detectable_maximum_nested_risk() {
        for (body, category) in [
            (
                "npm install alpha $(git reset --hard)",
                ShellCategory::DestructiveGit,
            ),
            (
                "git reset $(Remove-Item -Recurse tmp)",
                ShellCategory::MassDelete,
            ),
            (
                "npm install alpha $(Write-Output $(git push))",
                ShellCategory::PublishOrRemote,
            ),
        ] {
            for (command, shell) in [
                (body.to_owned(), Shell::Pwsh),
                (format!("pwsh -Command '{body}'"), Shell::Sh),
            ] {
                let result = analyze(&command, shell);
                assert_eq!(result.category, category, "{command}: {result:?}");
                assert!(result.uncertain, "{command}: {result:?}");
                assert!(!result.packages_exact, "{command}: {result:?}");
            }
        }
    }
    #[test]
    fn static_powershell_arguments_keep_exact_direct_and_nested_packages() {
        for (command, shell) in [
            ("npm install alpha beta@2", Shell::Pwsh),
            ("pwsh -Command 'npm install alpha beta@2'", Shell::Sh),
            ("pwsh -Command 'npm install alpha beta@2'", Shell::Pwsh),
            (r#"pwsh -Command "npm install alpha beta@2""#, Shell::Cmd),
        ] {
            let result = analyze(command, shell);
            assert_eq!(result.category, ShellCategory::DependencyInstall);
            assert_eq!(result.packages, ["alpha", "beta@2"]);
            assert!(result.packages_exact, "{command}: {result:?}");
            assert!(!result.uncertain, "{command}: {result:?}");
        }
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

#[cfg(test)]
mod sandbox_automation_tests {
    use super::*;
    #[test]
    fn automatic_shell_never_accepts_dangerous_uncertain_or_escalated_calls() {
        assert!(automatic_general_shell(r#"{"command":"echo ordinary"}"#));
        for command in [
            "npm install foo",
            "git reset --hard",
            "git push",
            "rm -rf data",
            "sudo echo x",
            "runas /user:admin echo",
            "su root",
            "pkexec sh",
        ] {
            assert!(
                !automatic_general_shell(&serde_json::json!({"command":command}).to_string()),
                "{command}"
            );
        }
        assert!(!automatic_general_shell(
            r#"{"command":"echo ordinary","sandbox_posture":"Verified"}"#
        ));
        assert!(!automatic_general_shell(
            r#"{"command":"echo ordinary","sandbox_permissions":"require_escalated"}"#
        ));
        assert!(!automatic_general_shell("invalid"));
    }

    #[test]
    fn redirects_are_uncertain_and_never_automatic_general_shell() {
        let redirect = r#"echo escaped > "/tmp/out.txt""#;
        let analysis = analyze(redirect, Shell::Sh);
        assert!(analysis.uncertain, "{analysis:?}");
        assert!(!analysis.packages_exact, "{analysis:?}");
        assert!(!automatic_general_shell(
            &serde_json::json!({"command": redirect}).to_string()
        ));

        for command in [
            redirect,
            "cat < /tmp/in.txt",
            "cat <<EOF\nhello\nEOF",
            "cat <<< hello",
        ] {
            let sh_analysis = analyze(command, Shell::Sh);
            assert!(sh_analysis.uncertain, "{command}: {sh_analysis:?}");
            assert!(
                !automatic_general_shell_for(
                    &serde_json::json!({"command": command}).to_string(),
                    Shell::Sh
                ),
                "{command}"
            );
        }

        let pwsh_analysis = analyze(redirect, Shell::Pwsh);
        assert!(pwsh_analysis.uncertain, "{pwsh_analysis:?}");
        assert!(!automatic_general_shell_for(
            &serde_json::json!({"command": redirect}).to_string(),
            Shell::Pwsh
        ));
    }

    #[test]
    fn long_running_dev_server_and_watch_commands_are_detected_without_false_positives() {
        let blocked = [
            "npm run dev",
            "npm run tauri dev",
            "npm start",
            "npm run watch:css",
            "pnpm dev",
            "pnpm tauri dev",
            "yarn watch",
            "bun --watch src/index.ts",
            "vite",
            "vite preview",
            "npx vite",
            "npx --yes next dev",
            "cargo watch -x test",
            "cargo tauri dev",
            "vitest",
            "npx vitest",
            "vitest --watch",
            "tsc --watch",
            "tsc -w",
            "jest --watch",
            "nodemon index.js",
            "python -m http.server 8000",
            "tail -f app.log",
            "docker compose up",
            "cmd /c npm run tauri dev",
            r#"sh -c "cargo watch -x check""#,
            r#"bash -lc "vite""#,
            "echo starting && npm run dev",
        ];
        for command in blocked {
            for shell in [Shell::Sh, Shell::Pwsh, Shell::Cmd] {
                let analysis = analyze(command, shell);
                assert!(
                    analysis.long_running,
                    "expected long_running=true for {shell:?}: {command} ({analysis:?})"
                );
                assert!(
                    blocked_long_running_command_for(command, shell).is_some(),
                    "expected blocked pattern for {shell:?}: {command}"
                );
                assert!(
                    !automatic_general_shell_for(
                        &serde_json::json!({"command": command}).to_string(),
                        shell
                    ),
                    "long-running command must never auto-execute ({shell:?}): {command}"
                );
            }
        }
        // `--` pass-through syntax on POSIX/CMD shells (and quoted `'--'` on PowerShell).
        for (command, shell) in [
            ("npm run tauri -- dev", Shell::Sh),
            ("npm run tauri -- dev", Shell::Cmd),
            ("npm run tauri '--' dev", Shell::Pwsh),
        ] {
            let analysis = analyze(command, shell);
            assert!(analysis.long_running, "{shell:?}: {command} ({analysis:?})");
        }
        assert!(analyze("npm run tauri -- dev", Shell::Pwsh).uncertain);

        let allowed = [
            "npm test",
            "npm run build",
            "npm run tauri build",
            "vite build",
            "npx vite build",
            "vite --version",
            "vitest run",
            "npx vitest run",
            "vitest --run src/app.test.ts",
            "cargo test",
            "cargo build",
            "cargo check",
            "cargo clippy",
            "tsc --noEmit",
            "npx tsc",
            "docker compose up -d",
            r#"echo "npm run dev""#,
            r#"git log --grep="cargo watch""#,
        ];
        for command in allowed {
            for shell in [Shell::Sh, Shell::Pwsh, Shell::Cmd] {
                let analysis = analyze(command, shell);
                assert!(
                    !analysis.long_running,
                    "unexpected long_running=true for {shell:?}: {command} ({analysis:?})"
                );
                assert!(
                    blocked_long_running_command_for(command, shell).is_none(),
                    "benign finite command must not be blocked ({shell:?}): {command}"
                );
            }
        }
    }

    #[test]
    fn stage7_audit_bash_background_process_substitution_and_bundled_flags_fail_closed() {
        for command in ["sleep 10 &", "cat <(echo hi)", "foo() { rm -rf tmp; }"] {
            let analysis = analyze(command, Shell::Sh);
            assert!(analysis.uncertain, "{command}: {analysis:?}");
            assert!(
                !automatic_general_shell_for(
                    &serde_json::json!({"command": command}).to_string(),
                    Shell::Sh
                ),
                "{command}"
            );
        }

        let bundled_git = analyze(r#"sh -ec "git reset --hard""#, Shell::Sh);
        assert_eq!(bundled_git.category, ShellCategory::DestructiveGit);
        let bundled_rm = analyze(r#"bash -lc "rm -rf tmp""#, Shell::Sh);
        assert_eq!(bundled_rm.category, ShellCategory::MassDelete);

        let cmd_k = analyze(r#"cmd /k "rm -rf tmp""#, Shell::Cmd);
        assert_eq!(cmd_k.category, ShellCategory::MassDelete);
        assert!(cmd_k.uncertain);

        let cmd_start = analyze("start /b echo hi", Shell::Cmd);
        assert!(cmd_start.uncertain);
        assert!(!automatic_general_shell_for(
            r#"{"command":"start /b echo hi"}"#,
            Shell::Cmd
        ));
    }
}
