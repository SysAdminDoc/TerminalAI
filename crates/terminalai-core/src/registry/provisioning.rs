//! Standing up and tearing down what a session's launch spec asked for: the
//! declared lease, and the worktree this tool provisions for it.
//!
//! Split out of `mod.rs` for size. Ordering is the part that matters and is
//! documented on each step — provisioning runs cheapest-first so a failure
//! leaves as little behind as possible, and teardown reverses it.

use super::*;

impl SessionRegistry {
    /// Provision one session's declared lease.
    ///
    /// Ordered cheapest-first so a failure leaves as little behind as possible:
    /// copying files cannot fail halfway in a way that needs undoing, while the
    /// database is the one step that creates state outside the working tree.
    pub(super) fn apply_lease(
        &self,
        id: &SessionId,
        lease: &lease::ResolvedLease,
        cwd: &std::path::Path,
        environment: &mut Vec<(String, String)>,
    ) -> Result<(), RegistryError> {
        // Copying is from the repository into itself for an in-place session,
        // which `copy_files` treats as a no-op; it matters once a session runs
        // in its own worktree.
        if !lease.copy.is_empty() {
            let source = self.lease_source(id).unwrap_or_else(|| cwd.to_path_buf());
            match lease::copy_files(&source, cwd, &lease.copy) {
                Ok(copied) if !copied.is_empty() => {
                    tracing::debug!(session = %id, "copied {} leased config file(s)", copied.len());
                }
                Ok(_) => {}
                Err(error) => {
                    return Err(RegistryError::Environment(EnvironmentError::HookSpawn {
                        phase: "lease-copy",
                        cause: error.to_string(),
                    }))
                }
            }
        }

        let admin_url = lease
            .database
            .as_ref()
            .and_then(|database| std::env::var(&database.admin_url_env).ok());
        if let (Some(database), Some(args)) = (&lease.database, lease.create_database_args()) {
            let Some(admin) = admin_url.as_deref() else {
                // Declared but unprovisioned is refused rather than skipped: a
                // session that quietly falls back to the shared database is the
                // exact collision this lease exists to prevent.
                return Err(RegistryError::Environment(EnvironmentError::HookSpawn {
                    phase: "lease-database",
                    cause: format!(
                        "{} declares a database lease but {} is not set",
                        lease::LEASE_FILE,
                        database.admin_url_env
                    ),
                }));
            };
            run_lease_command("psql", cwd, &args, Some(admin), "lease-database")?;
        }

        for (key, value) in lease.variables(admin_url.as_deref()) {
            environment.retain(|(existing, _)| existing != &key);
            environment.push((key, value));
        }
        Ok(())
    }

    /// Where leased config is copied from. `None` when the session runs in the
    /// repository itself and there is nothing to copy across.
    /// Where leased config is copied *from*.
    ///
    /// A worktree is a fresh checkout, so it has the tracked files and none of
    /// the untracked ones — which is exactly where `.env` and its neighbours
    /// live. Copying from the repository the checkout was cut from is what
    /// makes an isolated session actually runnable.
    fn lease_source(&self, id: &SessionId) -> Option<std::path::PathBuf> {
        let state = lock_state(&self.inner);
        state
            .entries
            .get(id)
            .and_then(|entry| entry.session.worktree.as_ref())
            .map(|worktree| worktree.repo.clone())
    }

    /// Attach the directory session worktrees are cut into.
    ///
    /// Unset means the registry has no place on disk it owns, so a session that
    /// asks for isolation is refused rather than checked out somewhere
    /// arbitrary. The daemon sets this; tests set it explicitly.
    pub fn set_worktree_root(&self, root: std::path::PathBuf) {
        if let Ok(mut slot) = self.inner.worktree_root.lock() {
            *slot = Some(root);
        }
    }

    /// Checkouts under the worktree root that no live session owns.
    ///
    /// Teardown deliberately keeps a branch holding unmerged work, which is
    /// right, but nothing ever revisited it — so worktrees, branches and their
    /// registrations accumulated silently. Reports rather than deletes: what to
    /// do about unmerged work is the operator's call, not the supervisor's.
    pub fn stale_worktrees(&self) -> Vec<crate::worktree::StaleWorktree> {
        let Some(root) = self
            .inner
            .worktree_root
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
        else {
            return Vec::new();
        };
        let live: Vec<crate::worktree::Worktree> = {
            let state = lock_state(&self.inner);
            state
                .entries
                .values()
                .filter_map(|entry| entry.session.worktree.clone())
                .collect()
        };
        crate::worktree::survey(&root, &live)
    }

    /// Create this session's worktree, if it asked for one.
    ///
    /// Runs on the worker thread that starts the session, because `git worktree
    /// add` copies a checkout and the launch call must not block on it.
    pub(super) fn provision_worktree(&self, id: &SessionId) -> Result<(), RegistryError> {
        let (wanted, cwd) = {
            let state = lock_state(&self.inner);
            match state.entries.get(id) {
                // Already provisioned — a restart of an existing session must
                // reuse its checkout, not cut a second one.
                Some(entry) if entry.session.worktree.is_some() => return Ok(()),
                Some(entry) => (entry.spec.worktree, entry.spec.cwd.clone()),
                None => return Err(RegistryError::Missing(id.clone())),
            }
        };
        if !wanted {
            return Ok(());
        }
        let root = self
            .inner
            .worktree_root
            .lock()
            .ok()
            .and_then(|root| root.clone())
            .ok_or_else(|| {
                RegistryError::Environment(EnvironmentError::HookSpawn {
                    phase: "worktree",
                    cause: "no directory is configured for session worktrees".to_owned(),
                })
            })?;
        let created = crate::worktree::create(&root, &cwd, &id.0).map_err(|error| {
            // Refused, never downgraded to a shared tree: a session the
            // operator asked to isolate that quietly runs in the repository is
            // the collision this feature exists to prevent.
            RegistryError::Environment(EnvironmentError::HookSpawn {
                phase: "worktree",
                cause: error.to_string(),
            })
        })?;
        tracing::info!(
            path = %created.path.display(),
            branch = %created.branch,
            "session worktree created"
        );
        let session = {
            let mut state = lock_state(&self.inner);
            let Some(entry) = state.entries.get_mut(id) else {
                // The session went away while git was working. Leaving the
                // checkout behind would orphan it, since nothing records it.
                drop(state);
                let failures = crate::worktree::remove(&created);
                if !failures.is_empty() {
                    tracing::warn!(?failures, "could not clean up an orphaned worktree");
                }
                return Err(RegistryError::Missing(id.clone()));
            };
            entry.session.cwd = created.path.clone();
            entry.session.branch = Some(created.branch.clone());
            entry.session.worktree = Some(created);
            entry.spec.cwd = entry.session.cwd.clone();
            entry.command.cwd = entry.session.cwd.clone();
            entry.session.clone()
        };
        self.emit(RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
        Ok(())
    }

    /// Remove a session's checkout, returning what could not be cleaned up.
    pub(super) fn release_worktree(&self, id: &SessionId) -> Vec<String> {
        let worktree = {
            let state = lock_state(&self.inner);
            state
                .entries
                .get(id)
                .and_then(|entry| entry.session.worktree.clone())
        };
        match worktree {
            Some(worktree) => crate::worktree::remove(&worktree),
            None => Vec::new(),
        }
    }

    /// Release a session's leased resources, returning every failure rather than
    /// the first: a compose stack that failed to come down must still be
    /// reported even if the database also failed to drop.
    pub(super) fn release_lease(&self, id: &SessionId, cwd: &std::path::Path) -> Vec<String> {
        let mut failures = Vec::new();
        let lease = match lease::Lease::load(cwd) {
            Ok(Some(lease)) => lease,
            Ok(None) => return failures,
            Err(error) => {
                failures.push(format!("lease could not be re-read for teardown: {error}"));
                return failures;
            }
        };
        let resolved = match lease.resolve(&id.0, cwd) {
            Ok(resolved) => resolved,
            Err(error) => {
                failures.push(format!("lease could not be resolved for teardown: {error}"));
                return failures;
            }
        };

        if let Some(args) = resolved.compose_down_args() {
            if let Err(error) = run_lease_command("docker", cwd, &args, None, "lease-compose") {
                failures.push(error.to_string());
            }
        }
        if let Some(args) = resolved.drop_database_args() {
            let database = resolved.database.as_ref().expect("args imply a database");
            match std::env::var(&database.admin_url_env) {
                Ok(admin) => {
                    if let Err(error) =
                        run_lease_command("psql", cwd, &args, Some(&admin), "lease-database")
                    {
                        failures.push(error.to_string());
                    }
                }
                Err(_) => failures.push(format!(
                    "session database {} could not be dropped: {} is not set",
                    database.name, database.admin_url_env
                )),
            }
        }
        failures
    }
}

/// Run one leased provisioning or teardown command.
///
/// `PGDATABASE`-style connection details are passed through the environment
/// rather than the argument vector so a connection string carrying a password
/// never appears in a process listing.
pub(super) fn run_lease_command(
    program: &str,
    cwd: &std::path::Path,
    args: &[String],
    connection: Option<&str>,
    phase: &'static str,
) -> Result<(), RegistryError> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let extra_environment = connection
        .map(|connection| {
            vec![
                // libpq treats PGDATABASE as its dbname default, including
                // URI/keyword connection strings. PGURI is not a libpq key.
                ("PGDATABASE".to_owned(), connection.to_owned()),
                ("PGCONNECT_TIMEOUT".to_owned(), "10".to_owned()),
            ]
        })
        .unwrap_or_default();
    environment::configure_command_environment(&mut command, &extra_environment);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command
        .output()
        .map_err(|error| EnvironmentError::HookSpawn {
            phase,
            cause: format!("could not run {program}: {error}"),
        })?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    Err(RegistryError::Environment(EnvironmentError::HookSpawn {
        phase,
        cause: if detail.is_empty() {
            format!("{program} exited with {}", output.status)
        } else {
            format!("{program}: {detail}")
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::tests::spool_scratch;

    #[test]
    fn lease_command_child_probe() {
        let cwd = std::env::current_dir().expect("probe cwd");
        let marker = cwd.join("terminalai-lease-command-probe.request");
        if !marker.exists() {
            return;
        }
        let report = serde_json::json!({
            "args": std::env::args().collect::<Vec<_>>(),
            "environment": std::env::vars().collect::<std::collections::BTreeMap<_, _>>(),
        });
        std::fs::write(
            cwd.join("terminalai-lease-command-probe.json"),
            serde_json::to_vec(&report).expect("encode probe report"),
        )
        .expect("write probe report");
    }

    #[test]
    fn lease_command_uses_the_allowlist_without_putting_connection_in_argv() {
        let scratch = spool_scratch("lease-command");
        std::fs::create_dir_all(&scratch.0).expect("scratch directory");
        std::fs::write(
            scratch.0.join("terminalai-lease-command-probe.request"),
            "probe",
        )
        .expect("probe marker");

        let connection = "postgresql://admin:password@127.0.0.1:5432/postgres?sslmode=require";
        let executable = std::env::current_exe().expect("test executable");
        run_lease_command(
            executable.to_str().expect("test executable path"),
            &scratch.0,
            &["lease_command_child_probe".to_owned()],
            Some(connection),
            "lease-test",
        )
        .expect("spawn lease command probe");

        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(scratch.0.join("terminalai-lease-command-probe.json"))
                .expect("probe report"),
        )
        .expect("decode probe report");
        let child_args = report["args"].as_array().expect("child argv");
        assert!(
            !child_args
                .iter()
                .any(|argument| argument.as_str() == Some(connection)),
            "connection string leaked into child argv: {child_args:?}"
        );

        let child_environment = report["environment"]
            .as_object()
            .expect("child environment");
        let allowed = environment::safe_environment_keys()
            .iter()
            .copied()
            .chain(["PGDATABASE", "PGCONNECT_TIMEOUT"])
            .collect::<std::collections::BTreeSet<_>>();
        let unexpected = child_environment
            .keys()
            // The profiling runtime sets its own variables inside the child,
            // after `env_clear()` has already run — so they are not inherited
            // and say nothing about the allowlist. Without this the suite cannot
            // run under `cargo llvm-cov` at all, and narrowing the assertion to
            // this one prefix keeps it strict about everything that is actually
            // passed in.
            .filter(|key| !key.starts_with("__LLVM_PROFILE") && key.as_str() != "LLVM_PROFILE_FILE")
            .filter(|key| !allowed.contains(key.as_str()))
            .collect::<Vec<_>>();
        assert!(
            unexpected.is_empty(),
            "lease command inherited unexpected environment keys: {unexpected:?}"
        );
        assert_eq!(
            child_environment["PGDATABASE"].as_str(),
            Some(connection)
        );
        assert_eq!(
            child_environment["PGCONNECT_TIMEOUT"].as_str(),
            Some("10")
        );
        assert!(!child_environment.contains_key("PGURI"));
    }
}
