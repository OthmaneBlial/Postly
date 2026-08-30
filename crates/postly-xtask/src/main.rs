use std::{
    env,
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    let command = env::args().nth(1).unwrap_or_else(|| "check".to_owned());
    let result = match command.as_str() {
        "fmt" => run("cargo", &["fmt", "--all", "--", "--check"]),
        "lint" => run(
            "cargo",
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ),
        "test" => run("cargo", &["test", "--workspace", "--all-targets"]),
        "check" => {
            run("cargo", &["fmt", "--all", "--", "--check"])
                && run(
                    "cargo",
                    &[
                        "clippy",
                        "--workspace",
                        "--all-targets",
                        "--",
                        "-D",
                        "warnings",
                    ],
                )
                && run("cargo", &["test", "--workspace", "--all-targets"])
        }
        "help" | "--help" => {
            println!("cargo xtask check|fmt|lint|test");
            true
        }
        other => {
            eprintln!("unknown xtask command: {other}");
            false
        }
    };
    if result {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run(program: &str, args: &[&str]) -> bool {
    eprintln!("$ {program} {}", args.join(" "));
    Command::new(program)
        .args(args)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
