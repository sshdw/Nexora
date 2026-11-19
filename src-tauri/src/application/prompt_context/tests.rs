use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::infrastructure::database::in_memory_database;

struct Fixture {
    root: PathBuf,
    db: Database,
    conversation: i64,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("nexora-prompt-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("app")).unwrap();
        fs::create_dir_all(root.join("project")).unwrap();
        let db = in_memory_database();
        let conversation = ConversationRepository::new(&db)
            .create("Prompt context", "active")
            .unwrap();
        Self {
            root,
            db,
            conversation,
        }
    }

    fn workspace(&self) -> PathBuf {
        self.root.join("project")
    }

    fn app_data(&self) -> PathBuf {
        self.root.join("app")
    }

    fn write(&self, filename: &str, content: &str) {
        let target = self.root.join(filename);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, content).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn project_skill(fixture: &Fixture, body: &str) {
    fixture.write(
        "project/.nexora/skills/check/SKILL.md",
        &format!("---\nname: check\ndescription: Check the supplied material\n---\n{body}"),
    );
}

fn first_skill(context: &PromptContext<'_>) -> serde_json::Value {
    serde_json::to_value(&context.skills(None).unwrap()[0]).unwrap()
}

#[test]
fn skills_require_pin_confirmation_and_invalidate_auto_opt_in_after_changes() {
    let fixture = Fixture::new();
    project_skill(&fixture, "First revision.");
    let app_data = fixture.app_data();
    let workspace = fixture.workspace();
    let context = PromptContext::new(
        &fixture.db,
        &app_data,
        Some(&workspace),
        fixture.conversation,
    )
    .unwrap();
    let summary = first_skill(&context);
    let reference: SkillRef = serde_json::from_value(summary["reference"].clone()).unwrap();
    let refs = TurnReferences {
        skills: vec![reference.clone()],
        ..Default::default()
    };
    assert_eq!(summary["trusted"], false);
    assert!(matches!(
        context.admit(&refs),
        Err(ContextError::SkillUnavailable)
    ));
    assert!(
        serde_json::to_string(&context.view_skill(&reference).unwrap())
            .unwrap()
            .contains("First revision.")
    );

    context
        .skill_preference(&reference, SkillAction::Trust)
        .unwrap();
    context
        .skill_preference(&reference, SkillAction::EnableAutoSelection)
        .unwrap();
    assert_eq!(context.skills(Some("Please check this.")).unwrap().len(), 1);
    assert!(context
        .admit(&refs)
        .unwrap()
        .unwrap()
        .contains("First revision."));
    context
        .skill_preference(&reference, SkillAction::Disable)
        .unwrap();
    assert!(matches!(
        context.admit(&refs),
        Err(ContextError::SkillUnavailable)
    ));
    context
        .skill_preference(&reference, SkillAction::Enable)
        .unwrap();

    project_skill(&fixture, "Second revision.");
    let summary = first_skill(&context);
    assert_eq!(summary["trusted"], false);
    assert_eq!(summary["auto_select"], false);
    assert!(matches!(context.admit(&refs), Err(ContextError::Stale)));
    let current = serde_json::from_value(summary["reference"].clone()).unwrap();
    context
        .skill_preference(&current, SkillAction::Trust)
        .unwrap();
    assert!(context
        .skills(Some("Please check this."))
        .unwrap()
        .is_empty());
}

#[test]
fn a_stale_split_pane_cannot_send_a_revoked_automatic_skill() {
    let fixture = Fixture::new();
    project_skill(&fixture, "Review the example.");
    let app = fixture.app_data();
    let workspace = fixture.workspace();
    let first =
        PromptContext::new(&fixture.db, &app, Some(&workspace), fixture.conversation).unwrap();
    let second_id = ConversationRepository::new(&fixture.db)
        .create("Split pane", "active")
        .unwrap();
    let second = PromptContext::new(&fixture.db, &app, Some(&workspace), second_id).unwrap();
    let reference = serde_json::from_value(first_skill(&first)["reference"].clone()).unwrap();
    first
        .skill_preference(&reference, SkillAction::Trust)
        .unwrap();
    first
        .skill_preference(&reference, SkillAction::EnableAutoSelection)
        .unwrap();
    assert_eq!(first.skills(Some("Please check.")).unwrap().len(), 1);
    let automatic = TurnReferences {
        automatic_skills: vec![reference.clone()],
        ..Default::default()
    };
    assert!(first.admit(&automatic).unwrap().is_some());

    second
        .skill_preference(&reference, SkillAction::DisableAutoSelection)
        .unwrap();
    assert!(matches!(
        first.admit(&automatic),
        Err(ContextError::AutoSelectionChanged)
    ));
    let explicit = TurnReferences {
        skills: vec![reference],
        ..Default::default()
    };
    assert!(first.admit(&explicit).unwrap().is_some());
}

#[test]
fn full_source_redaction_precedes_folders_excerpts_and_prompt_rendering() {
    let fixture = Fixture::new();
    let obfuscated = "s\u{202e}k-abcdefghijklmnop1234";
    let tail = "SYNTHETIC-FIXTURE-PEM-BODY";
    let sample = format!("first\r\n-----BEGIN PRIVATE KEY-----\r\nfixture\r\n{tail}\r\n-----END PRIVATE KEY-----\r\nfn example() {{}}\r\n{obfuscated}\r\n");
    fixture.write("project/src/example.txt", &sample);
    fixture.write(
        "app/skills/check/SKILL.md",
        &format!("---\nname: check\ndescription: {obfuscated}\n---\n{sample}"),
    );
    fixture.write("project/src/extra.txt", "Readable after the first file.\n");
    let app = fixture.app_data();
    let workspace = fixture.workspace();
    let context =
        PromptContext::new(&fixture.db, &app, Some(&workspace), fixture.conversation).unwrap();
    let summary = first_skill(&context);
    assert_eq!(summary["description"], "sk-[redacted]");
    let skill = serde_json::from_value(summary["reference"].clone()).unwrap();
    let skill_view = serde_json::to_value(context.view_skill(&skill).unwrap()).unwrap();
    let selectors = [
        ContextSelector::File {
            path: "src/example.txt".into(),
        },
        ContextSelector::Folder { path: "src".into() },
        ContextSelector::Symbol {
            path: "src/example.txt".into(),
            symbol: "example".into(),
        },
    ];
    let previews: Vec<_> = selectors
        .into_iter()
        .map(|selector| context.preview(selector).unwrap())
        .collect();
    assert!(previews[1].text.contains("Readable after the first file."));
    assert!(previews[2].text.contains("6: fn example() {}"));
    for symbol in ["redacted", "FIXTURE"] {
        assert!(matches!(
            context.preview(ContextSelector::Symbol {
                path: "src/example.txt".into(),
                symbol: symbol.into()
            }),
            Err(ContextError::ReadUnavailable)
        ));
    }
    let refs = TurnReferences {
        skills: vec![skill],
        contexts: previews.iter().map(|view| view.reference.clone()).collect(),
        ..Default::default()
    };
    let rendered = context.admit(&refs).unwrap().unwrap();
    let mut message = String::from("Read the examples.");
    add_to_user_message(&mut message, &rendered);
    assert!(message.ends_with(&rendered));
    let items: Vec<serde_json::Value> = serde_json::from_str(&rendered).unwrap();
    assert_eq!(items[0]["text"], skill_view["text"]);
    for (item, view) in items[1..].iter().zip(previews) {
        assert_eq!(item["source"], view.label);
        assert_eq!(item["text"], view.text);
    }
    for secret in [tail, "abcdefghijklmnop1234", "\u{202e}"] {
        assert!(!message.contains(secret));
    }
}

#[test]
fn context_refs_are_bound_to_content_and_conversation_and_redact_secrets() {
    let fixture = Fixture::new();
    fixture.write(
        "project/lib.txt",
        "fn answer() {}\n\nsecret sk-abcdefghijklmnop1234\n",
    );
    let app_data = fixture.app_data();
    let workspace = fixture.workspace();
    let context = PromptContext::new(
        &fixture.db,
        &app_data,
        Some(&workspace),
        fixture.conversation,
    )
    .unwrap();
    let selector = ContextSelector::Symbol {
        path: "lib.txt".into(),
        symbol: "answer".into(),
    };
    let view = context.preview(selector).unwrap();
    assert!(view.text.contains("fn answer()"));
    assert!(!view.text.contains("abcdefghijklmnop1234"));
    let refs = TurnReferences {
        contexts: vec![view.reference],
        ..Default::default()
    };
    assert!(context.admit(&refs).unwrap().unwrap().contains("answer"));
    let other = ConversationRepository::new(&fixture.db)
        .create("Another room", "active")
        .unwrap();
    let other = PromptContext::new(&fixture.db, &app_data, Some(&workspace), other).unwrap();
    assert!(matches!(other.admit(&refs), Err(ContextError::Stale)));

    fixture.write("project/lib.txt", "fn answer() { changed() }");
    assert!(matches!(context.admit(&refs), Err(ContextError::Stale)));
}

#[test]
fn workspace_context_refuses_ignored_sensitive_and_linked_files() {
    let fixture = Fixture::new();
    fixture.write("project/.nexora/.nexoraignore", "ignored.txt\n");
    fixture.write("project/.env", "should never appear");
    fixture.write("project/ignored.txt", "should never appear");
    fixture.write("project/allowed.txt", "allowed text");
    fixture.write("outside.txt", "should never appear");
    let workspace = fixture.workspace();
    fs::hard_link(
        fixture.root.join("outside.txt"),
        workspace.join("linked.txt"),
    )
    .unwrap();
    let app_data = fixture.app_data();
    let context = PromptContext::new(
        &fixture.db,
        &app_data,
        Some(&workspace),
        fixture.conversation,
    )
    .unwrap();
    for path in [
        ".env",
        "ignored.txt",
        "linked.txt",
        "../outside.txt",
        ".nexora/.nexoraignore",
    ] {
        assert!(
            context
                .preview(ContextSelector::File { path: path.into() })
                .is_err(),
            "{path}"
        );
    }
    let folder = context
        .preview(ContextSelector::Folder { path: ".".into() })
        .unwrap();
    assert!(folder.text.contains("allowed text"));
    assert!(!folder.text.contains("should never appear"));
}

fn commit_files(root: &Path, files: &[&str]) -> git2::Oid {
    let repo =
        git2::Repository::open(root).unwrap_or_else(|_| git2::Repository::init(root).unwrap());
    let mut index = repo.index().unwrap();
    for file in files {
        index.add_path(Path::new(file)).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Nexora test", "test@example.com").unwrap();
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "fixture",
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .unwrap()
}

#[test]
fn working_git_review_uses_tracked_safe_diff_and_blocks_external_object_store() {
    let fixture = Fixture::new();
    fixture.write("project/main.txt", "before\n");
    fixture.write("project/.env", "old hidden value\n");
    let root = fixture.workspace();
    commit_files(&root, &["main.txt", ".env"]);
    fixture.write(
        "project/main.txt",
        "after\ns\u{202e}k-abcdefghijklmnop1234\n",
    );
    fixture.write("project/.env", "new hidden value\n");
    fixture.write("project/untracked.txt", "untracked value\n");
    let app_data = fixture.app_data();
    let context =
        PromptContext::new(&fixture.db, &app_data, Some(&root), fixture.conversation).unwrap();
    let selector = ContextSelector::Diff { path: None };
    let preview = context.preview(selector.clone()).unwrap();
    assert!(
        preview.text.contains("-before") && preview.text.contains("+after"),
        "{}",
        preview.text
    );
    assert!(!preview.text.contains("hidden value") && !preview.text.contains("untracked value"));
    assert!(preview.text.contains("sk-[redacted]"));
    assert!(!preview.text.contains("abcdefghijklmnop1234"));
    let request = TurnReferences {
        contexts: vec![preview.reference],
        review: true,
        ..Default::default()
    };
    assert!(context.admit(&request).is_ok());
    assert!(
        fs::read_dir(&app_data).unwrap().next().is_none(),
        "scratch must be cleaned up"
    );

    fixture.write("project/.git/objects/info/alternates", "outside\n");
    assert!(matches!(
        context.preview(selector),
        Err(ContextError::ReadUnavailable)
    ));
}

#[test]
fn working_git_review_reads_a_normal_packed_repository() {
    let fixture = Fixture::new();
    fixture.write("project/main.txt", "before\n");
    let root = fixture.workspace();
    let commit_id = commit_files(&root, &["main.txt"]);
    let repo = git2::Repository::open(&root).unwrap();
    let tree = repo.find_commit(commit_id).unwrap().tree().unwrap();
    let objects = [
        commit_id,
        tree.id(),
        tree.get_name("main.txt").unwrap().id(),
    ];
    let mut pack = repo.packbuilder().unwrap();
    pack.insert_recursive(commit_id, None).unwrap();
    let git_objects = root.join(".git/objects");
    pack.write(&git_objects.join("pack"), 0).unwrap();
    for id in objects {
        let hex = id.to_string();
        fs::remove_file(git_objects.join(&hex[..2]).join(&hex[2..])).unwrap();
    }
    fixture.write("project/main.txt", "after\n");
    let app_data = fixture.app_data();
    let context =
        PromptContext::new(&fixture.db, &app_data, Some(&root), fixture.conversation).unwrap();
    let diff = context
        .preview(ContextSelector::Diff { path: None })
        .unwrap();
    assert!(diff.text.contains("-before\n+after"));
    assert!(fs::read_dir(app_data).unwrap().next().is_none());
}

#[test]
fn bounded_review_falls_back_to_a_single_tracked_file_in_large_repositories() {
    let fixture = Fixture::new();
    fixture.write("project/changed.txt", "first\n");
    let root = fixture.workspace();
    commit_files(&root, &["changed.txt"]);
    fixture.write("project/changed.txt", "second\n");
    fixture.write("project/untracked.txt", "not eligible\n");

    let repo = git2::Repository::open(&root).unwrap();
    let mut index = repo.index().unwrap();
    for number in 0..1024 {
        let name = format!("extra-{number:04}.txt");
        fs::write(root.join(&name), "synthetic\n").unwrap();
        index.add_path(Path::new(&name)).unwrap();
    }
    index.write().unwrap();
    drop(index);
    drop(repo);

    let app = fixture.app_data();
    let context = PromptContext::new(&fixture.db, &app, Some(&root), fixture.conversation).unwrap();
    for committed in [false, true] {
        if committed {
            commit_files(&root, &[]);
        }
        assert!(matches!(
            context.preview(ContextSelector::Diff { path: None }),
            Err(ContextError::DiffLimit)
        ));
        let preview = context
            .preview(ContextSelector::Diff {
                path: Some("changed.txt".into()),
            })
            .unwrap();
        assert!(preview.text.contains("-first\n+second"));
        assert_eq!(serde_json::to_value(&preview).unwrap()["omitted"], false);
        let refs = TurnReferences {
            contexts: vec![preview.reference],
            review: true,
            ..Default::default()
        };
        assert!(context
            .admit(&refs)
            .unwrap()
            .unwrap()
            .contains("changed.txt"));
        assert!(matches!(
            context.preview(ContextSelector::Diff {
                path: Some("untracked.txt".into())
            }),
            Err(ContextError::NoDiff)
        ));
    }
}

#[test]
fn a_new_git_head_invalidates_skill_trust_even_when_text_does_not_change() {
    let fixture = Fixture::new();
    project_skill(&fixture, "Pinned skill.");
    let workspace = fixture.workspace();
    let pin = commit_files(&workspace, &[".nexora/skills/check/SKILL.md"]);
    let app_data = fixture.app_data();
    let context = PromptContext::new(
        &fixture.db,
        &app_data,
        Some(&workspace),
        fixture.conversation,
    )
    .unwrap();
    let summary = first_skill(&context);
    assert_eq!(summary["commit"], pin.to_string());
    let reference = serde_json::from_value(summary["reference"].clone()).unwrap();
    context
        .skill_preference(&reference, SkillAction::Trust)
        .unwrap();
    fixture.write("project/other.txt", "New commit");
    commit_files(&workspace, &["other.txt"]);
    let new = first_skill(&context);
    assert_eq!(new["sha256"], summary["sha256"]);
    assert_eq!(new["trusted"], false);
    assert!(matches!(
        context.view_skill(&reference),
        Err(ContextError::Stale)
    ));
}
