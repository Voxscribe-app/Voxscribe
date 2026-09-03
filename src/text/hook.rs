//! User-supplied post-transcription hook.
//!
//! The command is user-authored config, so it runs through a shell on purpose;
//! pipes and chains are the point. Any failure passes the original text
//! through - a broken hook must never eat a dictation.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

pub struct HookContext<'a> {
    pub backend: &'a str,
    pub model: &'a str,
    pub language: &'a str,
}

pub async fn run(command: &str, text: &str, timeout: Duration, ctx: HookContext<'_>) -> String {
    if command.trim().is_empty() || text.is_empty() {
        return text.to_string();
    }

    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("DUSKR_BACKEND", ctx.backend)
        .env("DUSKR_MODEL", ctx.model)
        .env("DUSKR_LANGUAGE", ctx.language)
        // Kept so hooks written for hyprwhspr keep working after migration.
        .env("HYPRWHSPR_BACKEND", ctx.backend)
        .env("HYPRWHSPR_MODEL", ctx.model)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!("post_hook failed to start: {err}");
            return text.to_string();
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let payload = text.to_string();
        tokio::spawn(async move {
            let _ = stdin.write_all(payload.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }

    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            tracing::warn!("post_hook failed: {err}");
            return text.to_string();
        }
        Err(_) => {
            tracing::warn!("post_hook timed out after {:?}", timeout);
            return text.to_string();
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!("post_hook exited {}: {}", output.status, stderr.trim());
        return text.to_string();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim_end_matches(['\r', '\n']);
    // Empty stdout means the hook was an observer, not a rewriter.
    if trimmed.is_empty() {
        text.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> HookContext<'static> {
        HookContext {
            backend: "test",
            model: "test",
            language: "en",
        }
    }

    #[tokio::test]
    async fn non_empty_stdout_replaces_the_transcript() {
        let out = run("tr a-z A-Z", "hello", Duration::from_secs(5), ctx()).await;
        assert_eq!(out, "HELLO");
    }

    #[tokio::test]
    async fn observer_hooks_leave_the_transcript_alone() {
        let out = run("cat > /dev/null", "hello", Duration::from_secs(5), ctx()).await;
        assert_eq!(out, "hello");
    }

    #[tokio::test]
    async fn failing_hooks_pass_the_original_text_through() {
        let out = run("exit 3", "hello", Duration::from_secs(5), ctx()).await;
        assert_eq!(out, "hello");
    }

    #[tokio::test]
    async fn a_hanging_hook_is_bounded_by_the_timeout() {
        let out = run("sleep 30", "hello", Duration::from_millis(150), ctx()).await;
        assert_eq!(out, "hello");
    }

    #[tokio::test]
    async fn hook_environment_exposes_backend_and_model() {
        let out = run(
            "printf %s \"$DUSKR_BACKEND/$DUSKR_MODEL\"",
            "x",
            Duration::from_secs(5),
            ctx(),
        )
        .await;
        assert_eq!(out, "test/test");
    }
}
