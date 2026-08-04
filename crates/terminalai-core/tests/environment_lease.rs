//! Repository-declared environment leases.
//!
//! Worktrees isolate files and nothing else, so these cover the three things
//! that actually collide between parallel sessions: untracked config, docker
//! compose project names, and databases.

use std::path::{Path, PathBuf};

use terminalai_core::lease::{copy_files, matching_files, session_database_url, Lease, LEASE_FILE};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!(
        "terminalai-lease-{}-{name}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    Scratch(dir)
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    std::fs::write(path, contents).expect("write");
}

#[test]
fn a_repository_with_no_lease_declares_none() {
    let root = scratch("absent");
    assert_eq!(Lease::load(&root.0).expect("readable"), None);
}

#[test]
fn a_malformed_lease_is_refused_rather_than_ignored() {
    // Ignoring it would start a session that looks isolated and shares a
    // database — the exact failure the lease exists to prevent.
    let root = scratch("malformed");
    write(&root.0, LEASE_FILE, "copy = [oops");
    assert!(Lease::load(&root.0).is_err());
}

#[test]
fn a_lease_declares_copies_compose_and_a_database() {
    let lease = Lease::parse(
        r#"
        copy = [".env", "config/*.local.json"]

        [compose]
        project_prefix = "shop"
        file = "docker-compose.yml"
        remove_volumes = true

        [database]
        template = "shop_dev"
        name_prefix = "shop"
        "#,
    )
    .expect("valid lease");

    assert_eq!(lease.copy, vec![".env", "config/*.local.json"]);
    let compose = lease.compose.as_ref().expect("compose section");
    assert_eq!(compose.project_prefix.as_deref(), Some("shop"));
    assert!(
        !compose.remove_volumes,
        "repository metadata cannot remove volumes"
    );
    let database = lease.database.as_ref().expect("database section");
    assert_eq!(database.template, "shop_dev");
    // Defaults that keep a password out of a committed file.
    assert_eq!(database.admin_url_env, "TERMINALAI_DB_ADMIN_URL");
    assert_eq!(database.session_url_env, "DATABASE_URL");
    assert!(database.drop_on_teardown);
}

#[test]
fn volumes_are_not_removed_unless_asked() {
    // Destroying data the operator never asked to lose is not a default.
    let lease = Lease::parse("[compose]\nproject_prefix = \"shop\"\n").expect("valid");
    assert!(!lease.compose.expect("compose").remove_volumes);
}

#[test]
fn a_copy_glob_cannot_escape_the_repository() {
    for escape in [
        "copy = [\"../../secrets/.env\"]",
        "copy = [\"/etc/passwd\"]",
        "copy = [\"\\\\Windows\\\\System32\\\\.env\"]",
        "copy = [\"C:temp/.env\"]",
        "copy = [\"config/../../outside\"]",
    ] {
        assert!(
            Lease::parse(escape).is_err(),
            "{escape} should be refused"
        );
    }
}

#[test]
fn a_database_name_that_is_not_a_plain_identifier_is_refused() {
    // CREATE DATABASE takes no bind parameters, so the name is concatenated into
    // the statement; refusing anything but a plain identifier is the only guard.
    for hostile in [
        "template = \"shop\\\"; DROP DATABASE prod; --\"",
        "template = \"shop dev\"",
        "template = \"1shop\"",
        "template = \"\"",
    ] {
        let source = format!("[database]\n{hostile}\n");
        assert!(
            Lease::parse(&source).is_err(),
            "{hostile} should be refused"
        );
    }
}

#[test]
fn a_lease_cannot_name_an_arbitrary_environment_variable() {
    for field in ["admin_url_env", "session_url_env"] {
        let source = format!(
            "[database]\ntemplate = \"shop_dev\"\n{field} = \"PATH\"\n"
        );
        assert!(
            Lease::parse(&source).is_err(),
            "{field} must not shadow the process PATH"
        );
    }

    assert!(
        Lease::parse(
            "[database]\ntemplate = \"shop_dev\"\nadmin_url_env = \"DATABASE_URL\"\n"
        )
        .is_err(),
        "the daemon must not read a repository-selected general-purpose variable"
    );
    assert!(
        Lease::parse(
            "[database]\ntemplate = \"shop_dev\"\nsession_url_env = \"DATABASE_URL\"\n"
        )
        .is_ok(),
        "the documented session URL default remains valid"
    );
}

#[test]
fn two_sessions_get_distinct_compose_projects_and_databases() {
    // The whole point: the same declaration must produce non-colliding names.
    let lease = Lease::parse(
        "[compose]\nproject_prefix = \"shop\"\n\n[database]\ntemplate = \"shop_dev\"\n",
    )
    .expect("valid");
    let root = Path::new("C:/repos/shop");

    let first = lease.resolve("s0001", root).expect("first resolve");
    let second = lease.resolve("s0002", root).expect("second resolve");
    assert_eq!(first.compose_project.as_deref(), Some("shop-s0001"));
    assert_eq!(second.compose_project.as_deref(), Some("shop-s0002"));
    assert_ne!(first.compose_project, second.compose_project);
    assert_eq!(first.database.as_ref().expect("db").name, "shop_dev_s0001");
    assert_eq!(second.database.as_ref().expect("db").name, "shop_dev_s0002");
}

#[test]
fn the_compose_prefix_defaults_to_the_repository_folder() {
    let lease = Lease::parse("[compose]\n").expect("valid");
    let resolved = lease
        .resolve("s0007", Path::new("C:/repos/My Shop"))
        .expect("resolve");
    // Uppercase and spaces are not legal in a compose project name.
    assert_eq!(resolved.compose_project.as_deref(), Some("my-shop-s0007"));
}

#[test]
fn provisioning_statements_name_this_session_only() {
    let lease =
        Lease::parse("[database]\ntemplate = \"shop_dev\"\n").expect("valid");
    let resolved = lease
        .resolve("s0003", Path::new("C:/repos/shop"))
        .expect("resolve");

    let create = resolved.create_database_args().expect("create args");
    assert!(create.contains(&"ON_ERROR_STOP=1".to_owned()), "{create:?}");
    let statement = create.last().expect("statement");
    assert_eq!(
        statement,
        "CREATE DATABASE \"shop_dev_s0003\" TEMPLATE \"shop_dev\""
    );

    let drop = resolved.drop_database_args().expect("drop args");
    let statement = drop.last().expect("statement");
    // FORCE disconnects whatever the agent left open; without it teardown fails
    // every time a session exits with a live connection.
    assert_eq!(
        statement,
        "DROP DATABASE IF EXISTS \"shop_dev_s0003\" WITH (FORCE)"
    );
}

#[test]
fn a_database_kept_on_purpose_is_not_dropped() {
    let lease = Lease::parse("[database]\ntemplate = \"shop_dev\"\ndrop_on_teardown = false\n")
        .expect("valid");
    let resolved = lease
        .resolve("s0004", Path::new("C:/repos/shop"))
        .expect("resolve");
    assert!(resolved.create_database_args().is_some());
    assert_eq!(resolved.drop_database_args(), None);
}

#[test]
fn compose_teardown_targets_this_session_and_nothing_else() {
    let root = scratch("compose-down");
    write(&root.0, "ops/compose.yml", "services: {}\n");
    let lease = Lease::parse(
        "[compose]\nproject_prefix = \"shop\"\nfile = \"ops/compose.yml\"\nremove_volumes = true\n",
    )
    .expect("valid");
    let args = lease
        .resolve("s0005", &root.0)
        .expect("resolve")
        .compose_down_args()
        .expect("down args");
    let compose_file = std::fs::canonicalize(root.0.join("ops/compose.yml"))
        .expect("canonical compose file")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        args,
        vec![
            "compose",
            "--project-name",
            "shop-s0005",
            "--file",
            compose_file.as_str(),
            "down",
            "--remove-orphans",
        ]
    );
}

#[test]
fn a_compose_file_cannot_escape_the_repository() {
    for file in ["../../ops/prod.yml", "C:/ops/prod.yml"] {
        let source = format!("[compose]\nfile = \"{file}\"\n");
        assert!(
            Lease::parse(&source).is_err(),
            "compose file {file:?} should be refused"
        );
    }

    let root = scratch("compose-safe");
    write(&root.0, "ops/compose.yml", "services: {}\n");
    let lease = Lease::parse("[compose]\nfile = \"ops/compose.yml\"\nremove_volumes = true\n")
        .expect("valid");
    let args = lease
        .resolve("s0006", &root.0)
        .expect("resolve")
        .compose_down_args()
        .expect("down args");
    assert!(
        !args.iter().any(|arg| arg == "--volumes"),
        "repository metadata enabled destructive volume removal: {args:?}"
    );
}

#[test]
fn the_session_database_url_replaces_only_the_database() {
    assert_eq!(
        session_database_url("postgres://user:pw@localhost:5432/postgres", "shop_s0001").as_deref(),
        Some("postgres://user:pw@localhost:5432/shop_s0001")
    );
    // Query parameters (sslmode, and friends) survive.
    assert_eq!(
        session_database_url("postgresql://h/postgres?sslmode=require", "s").as_deref(),
        Some("postgresql://h/s?sslmode=require")
    );
    // Anything unrecognisable yields None rather than a plausible-looking URL
    // that would point the session at the wrong server.
    for bad in ["mysql://h/db", "not a url", "", "postgres://"] {
        assert_eq!(session_database_url(bad, "s"), None, "{bad:?}");
    }
}

#[test]
fn copy_globs_select_untracked_config_without_walking_the_world() {
    let root = scratch("copy");
    let source = root.0.join("source");
    write(&source, ".env", "SECRET=1\n");
    write(&source, "config/app.local.json", "{}\n");
    write(&source, "config/app.json", "{}\n");
    write(&source, "node_modules/pkg/.env", "nope\n");
    write(&source, ".git/config", "nope\n");

    let matched = matching_files(
        &source,
        &[".env".to_owned(), "config/*.local.json".to_owned()],
    )
    .expect("globs match");
    let names: Vec<String> = matched
        .iter()
        .map(|path| {
            path.strip_prefix(&source)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(names, vec![".env", "config/app.local.json"]);
}

#[test]
fn a_recursive_glob_skips_dependency_and_build_directories() {
    let root = scratch("recursive");
    let source = root.0.join("source");
    write(&source, "services/api/.env", "one\n");
    write(&source, "services/web/.env", "two\n");
    write(&source, "node_modules/dep/.env", "never\n");
    write(&source, "target/debug/.env", "never\n");

    let matched = matching_files(&source, &["**/.env".to_owned()]).expect("globs match");
    let names: Vec<String> = matched
        .iter()
        .map(|path| {
            path.strip_prefix(&source)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(names, vec!["services/api/.env", "services/web/.env"]);
}

#[test]
fn copying_never_overwrites_what_the_session_already_has() {
    // The agent may already have edited the file. Overwriting its work to
    // satisfy a lease would be a failure of its own.
    let root = scratch("no-clobber");
    let source = root.0.join("source");
    let destination = root.0.join("worktree");
    write(&source, ".env", "from-source\n");
    write(&source, "extra.local", "new\n");
    write(&destination, ".env", "already-edited\n");

    let copied = copy_files(
        &source,
        &destination,
        &[".env".to_owned(), "*.local".to_owned()],
    )
    .expect("copy");
    assert_eq!(
        copied
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect::<Vec<_>>(),
        vec!["extra.local"]
    );
    assert_eq!(
        std::fs::read_to_string(destination.join(".env")).expect("kept"),
        "already-edited\n"
    );
}

#[test]
fn copying_into_the_same_directory_does_nothing() {
    let root = scratch("same-dir");
    write(&root.0, ".env", "x\n");
    assert_eq!(
        copy_files(&root.0, &root.0, &[".env".to_owned()]).expect("copy"),
        Vec::<PathBuf>::new()
    );
}
