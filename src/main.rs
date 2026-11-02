mod args;
mod command;
mod config;
mod label;
mod lock;
mod osc;
mod osc_watch;
mod proto;
mod socket;
mod state;
mod watch;

fn main() {
    // This process inherits the pane's tty. Anything written to stdout or stderr
    // lands in the middle of the user's shell session, so a bad invocation exits
    // silently rather than printing usage.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = match args::invocation(&argv) {
        Some(args::Invocation::Watch(a)) => watch::run(a),
        Some(args::Invocation::Finish(f)) => watch::finish(f),
        None => 0,
    };
    std::process::exit(code);
}
