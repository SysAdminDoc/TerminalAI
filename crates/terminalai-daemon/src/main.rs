fn main() {
    if let Err(error) = terminalai_daemon::run() {
        eprintln!("terminalai-daemon: {error}");
        std::process::exit(1);
    }
}
