use std::{env, path::PathBuf, process};

use gfcut_mcp::McpServer;

fn parse_project_path() -> PathBuf {
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--project" || arg == "-p" {
            if let Some(path) = args.next() {
                return PathBuf::from(path);
            }
        } else if let Some(path) = arg.strip_prefix("--project=") {
            return PathBuf::from(path);
        }
    }

    if let Ok(env_path) = env::var("GFCUT_PROJECT_PATH") {
        if !env_path.is_empty() {
            return PathBuf::from(env_path);
        }
    }

    PathBuf::from("demo.gfcut.json")
}

fn main() {
    let project_path = parse_project_path();
    let mut server = McpServer::new(project_path);

    if let Err(err) = server.run_stdio() {
        eprintln!("gfcut-mcp fatal stdio error: {err}");
        process::exit(1);
    }
}
