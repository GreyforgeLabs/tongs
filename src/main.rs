//! `tongs` command-line interface.

mod cli;

fn main() {
    let code = cli::main(std::env::args_os().skip(1).collect());
    std::process::exit(code);
}
