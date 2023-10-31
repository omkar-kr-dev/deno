// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use crate::emit::Emitter;
use crate::util::file_watcher::WatcherCommunicator;
use crate::util::file_watcher::WatcherRestartMode;
use deno_core::error::generic_error;
use deno_core::error::AnyError;
use deno_core::futures::StreamExt;
use deno_core::serde_json::json;
use deno_core::serde_json::{self};
use deno_core::url::Url;
use deno_core::LocalInspectorSession;
use deno_runtime::colors;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::select;

mod json_types;

use json_types::RpcNotification;
use json_types::ScriptParsed;
use json_types::SetScriptSourceReturnObject;
use json_types::Status;

/// This structure is responsible for providing Hot Module Replacement
/// functionality.
///
/// It communicates with V8 inspector over a local session and waits for
/// notifications about changed files from the `FileWatcher`.
///
/// Upon receiving such notification, the runner decides if the changed
/// path should be handled the `FileWatcher` itself (as if we were running
/// in `--watch` mode), or if the path is eligible to be hot replaced in the
/// current program.
///
/// Even if the runner decides that a path will be hot-replaced, the V8 isolate
/// can refuse to perform hot replacement, eg. a top-level variable/function
/// of an ES module cannot be hot-replaced. In such situation the runner will
/// force a full restart of a program by notifying the `FileWatcher`.
pub struct HmrRunner {
  session: LocalInspectorSession,
  watcher_communicator: Arc<WatcherCommunicator>,
  script_ids: HashMap<String, String>,
  emitter: Arc<Emitter>,
}

impl HmrRunner {
  pub fn new(
    emitter: Arc<Emitter>,
    session: LocalInspectorSession,
    watcher_communicator: Arc<WatcherCommunicator>,
  ) -> Self {
    Self {
      session,
      emitter,
      watcher_communicator,
      script_ids: HashMap::new(),
    }
  }

  // TODO(bartlomieju): this code is duplicated in `cli/tools/coverage/mod.rs`
  pub async fn start(&mut self) -> Result<(), AnyError> {
    self.enable_debugger().await
  }

  // TODO(bartlomieju): this code is duplicated in `cli/tools/coverage/mod.rs`
  pub async fn stop(&mut self) -> Result<(), AnyError> {
    self
      .watcher_communicator
      .change_restart_mode(WatcherRestartMode::Automatic);
    self.disable_debugger().await
  }

  // TODO(bartlomieju): this code is duplicated in `cli/tools/coverage/mod.rs`
  async fn enable_debugger(&mut self) -> Result<(), AnyError> {
    self
      .session
      .post_message::<()>("Debugger.enable", None)
      .await?;
    self
      .session
      .post_message::<()>("Runtime.enable", None)
      .await?;
    Ok(())
  }

  // TODO(bartlomieju): this code is duplicated in `cli/tools/coverage/mod.rs`
  async fn disable_debugger(&mut self) -> Result<(), AnyError> {
    self
      .session
      .post_message::<()>("Debugger.disable", None)
      .await?;
    self
      .session
      .post_message::<()>("Runtime.disable", None)
      .await?;
    Ok(())
  }

  async fn set_script_source(
    &mut self,
    script_id: &str,
    source: &str,
  ) -> Result<SetScriptSourceReturnObject, AnyError> {
    let result = self
      .session
      .post_message(
        "Debugger.setScriptSource",
        Some(json!({
          "scriptId": script_id,
          "scriptSource": source,
          "allowTopFrameEditing": true,
        })),
      )
      .await?;

    Ok(serde_json::from_value::<SetScriptSourceReturnObject>(
      result,
    )?)
  }

  async fn dispatch_hmr_event(
    &mut self,
    script_id: &str,
  ) -> Result<(), AnyError> {
    let expr = format!(
      "dispatchEvent(new CustomEvent(\"hmr\", {{ detail: {{ path: \"{}\" }} }}));",
      script_id
    );

    let _result = self
      .session
      .post_message(
        "Runtime.evaluate",
        Some(json!({
          "expression": expr,
          "contextId": Some(1),
        })),
      )
      .await?;

    Ok(())
  }

  pub async fn run(&mut self) -> Result<(), AnyError> {
    self
      .watcher_communicator
      .change_restart_mode(WatcherRestartMode::Manual);
    let mut session_rx = self.session.take_notification_rx();
    loop {
      select! {
        biased;
        Some(notification) = session_rx.next() => {
          let notification = serde_json::from_value::<RpcNotification>(notification)?;
          // TODO(bartlomieju): this is not great... and the code is duplicated with the REPL.
          if notification.method == "Runtime.exceptionThrown" {
            let params = notification.params;
            let exception_details = params.get("exceptionDetails").unwrap().as_object().unwrap();
            let text = exception_details.get("text").unwrap().as_str().unwrap();
            let exception = exception_details.get("exception").unwrap().as_object().unwrap();
            let description = exception.get("description").and_then(|d| d.as_str()).unwrap_or("undefined");
            break Err(generic_error(format!("{text} {description}")));
          } else if notification.method == "Debugger.scriptParsed" {
            let params = serde_json::from_value::<ScriptParsed>(notification.params)?;
            if params.url.starts_with("file://") {
              let file_url = Url::parse(&params.url).unwrap();
              let file_path = file_url.to_file_path().unwrap();
              if let Ok(canonicalized_file_path) = file_path.canonicalize() {
                let canonicalized_file_url = Url::from_file_path(canonicalized_file_path).unwrap();
                self.script_ids.insert(canonicalized_file_url.to_string(), params.script_id);
              }
            }
          }
        }
        changed_paths = self.watcher_communicator.watch_for_changed_paths() => {
          let changed_paths = changed_paths?;

          let Some(changed_paths) = changed_paths else {
            let _ = self.watcher_communicator.force_restart();
            continue;
          };

          let filtered_paths: Vec<PathBuf> = changed_paths.into_iter().filter(|p| p.extension().map_or(false, |ext| {
            let ext_str = ext.to_str().unwrap();
            matches!(ext_str, "js" | "ts" | "jsx" | "tsx")
          })).collect();

          // If after filtering there are no paths it means it's either a file
          // we can't HMR or an external file that was passed explicitly to
          // `--unstable-hmr=<file>` path.
          if filtered_paths.is_empty() {
            let _ = self.watcher_communicator.force_restart();
            continue;
          }

          for path in filtered_paths {
            let Some(path_str) = path.to_str() else {
              let _ = self.watcher_communicator.force_restart();
              continue;
            };
            let Ok(module_url) = Url::from_file_path(path_str) else {
              let _ = self.watcher_communicator.force_restart();
              continue;
            };

            let Some(id) = self.script_ids.get(module_url.as_str()).cloned() else {
              let _ = self.watcher_communicator.force_restart();
              continue;
            };

            let source_code = self.emitter.load_and_emit_for_hmr(
              &module_url
            ).await?;

            let mut tries = 1;
            loop {
              let result = self.set_script_source(&id, source_code.as_str()).await?;

              if matches!(result.status, Status::Ok) {
                self.dispatch_hmr_event(module_url.as_str()).await?;
                self.watcher_communicator.print(format!("Replaced changed module {}", module_url.as_str()));
                break;
              }

              self.watcher_communicator.print(format!("Failed to reload module {}: {}.", module_url, colors::gray(result.status.explain())));
              if result.status.should_retry() && tries <= 2 {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
              }

              let _ = self.watcher_communicator.force_restart();
              break;
            }
          }
        }
        _ = self.session.receive_from_v8_session() => {}
      }
    }
  }
}
