#![allow(clippy::needless_pass_by_value)] // Tauri owns deserialized arguments.

use std::path::Path;

use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::application::prompt_context::{
    ContextError, ContextSelector, ContextView, PromptContext, SkillAction, SkillRef, SkillSummary,
    SkillView, TurnReferences,
};
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};

impl From<ContextError> for CommandError {
    fn from(error: ContextError) -> Self {
        let kind = match error {
            ContextError::Settings => ErrorKind::Database,
            ContextError::ReadUnavailable | ContextError::NoDiff => ErrorKind::Io,
            _ => ErrorKind::InvalidInput,
        };
        Self::new(kind, error.to_string())
    }
}

fn with_context<T>(
    app: &AppHandle,
    db: &Database,
    conversation_id: i64,
    workspace: &Path,
    use_context: impl FnOnce(PromptContext<'_>) -> Result<T, ContextError>,
) -> Result<T, CommandError> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|_| CommandError::new(ErrorKind::Io, "Application data is unavailable."))?;
    let context = PromptContext::new(db, &base, Some(workspace), conversation_id)?;
    use_context(context).map_err(Into::into)
}

async fn background<T: Send + 'static>(
    app: AppHandle,
    conversation_id: i64,
    work: impl FnOnce(PromptContext<'_>) -> Result<T, ContextError> + Send + 'static,
) -> Result<T, CommandError> {
    tauri::async_runtime::spawn_blocking(move || {
        let db = app.state::<Database>();
        let workspace = super::agent::workspace_root(&app, db.inner())?;
        with_context(&app, db.inner(), conversation_id, &workspace, work)
    })
    .await
    .map_err(|_| CommandError::new(ErrorKind::Io, "Prompt context is unavailable."))?
}

#[tauri::command]
pub(crate) async fn list_prompt_skills(
    app: AppHandle,
    conversation_id: i64,
    draft: Option<String>,
) -> Result<Vec<SkillSummary>, CommandError> {
    background(app, conversation_id, move |context| {
        context.skills(draft.as_deref())
    })
    .await
}

#[tauri::command]
pub(crate) async fn view_prompt_skill(
    app: AppHandle,
    conversation_id: i64,
    reference: SkillRef,
) -> Result<SkillView, CommandError> {
    background(app, conversation_id, move |context| {
        context.view_skill(&reference)
    })
    .await
}

#[tauri::command]
pub(crate) async fn update_prompt_skill(
    app: AppHandle,
    conversation_id: i64,
    reference: SkillRef,
    action: SkillAction,
    locale: Option<String>,
) -> Result<SkillSummary, CommandError> {
    if matches!(action, SkillAction::Trust) {
        let russian = locale.as_deref() == Some("ru");
        let preview_ref = reference.clone();
        let native = app.clone();
        let confirmed = background(app.clone(), conversation_id, move |context| {
            let preview = context.view_skill(&preview_ref)?;
            let (allow, cancel) = if russian {
                ("Разрешить", "Отмена")
            } else {
                ("Allow", "Cancel")
            };
            Ok(native
                .dialog()
                .message(preview.trust_review(russian))
                .title("Nexora")
                .kind(MessageDialogKind::Warning)
                .buttons(MessageDialogButtons::OkCancelCustom(
                    allow.to_owned(),
                    cancel.to_owned(),
                ))
                .blocking_show())
        })
        .await?;
        if !confirmed {
            return Err(CommandError::new(
                ErrorKind::ConfirmationRequired,
                "Skill trust was not confirmed.",
            ));
        }
    }
    background(app, conversation_id, move |context| {
        context.skill_preference(&reference, action)
    })
    .await
}

#[tauri::command]
pub(crate) async fn preview_prompt_context(
    app: AppHandle,
    conversation_id: i64,
    selector: ContextSelector,
) -> Result<ContextView, CommandError> {
    background(app, conversation_id, move |context| {
        context.preview(selector)
    })
    .await
}

pub(super) fn admit(
    app: &AppHandle,
    db: &Database,
    conversation_id: i64,
    workspace: &Path,
    references: Option<&TurnReferences>,
    review_allowed: bool,
) -> Result<Option<String>, CommandError> {
    match references {
        Some(references) if !references.is_empty() => {
            if references.review() && !review_allowed {
                return Err(ContextError::Invalid.into());
            }
            with_context(app, db, conversation_id, workspace, |context| {
                context.admit(references)
            })
        }
        _ => Ok(None),
    }
}
