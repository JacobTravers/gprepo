use anyhow::{Context, Result};
use clap::{Arg, Command};
use git2::{Repository, StatusOptions, StatusShow};
use globset::GlobSetBuilder;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write, stdout};
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use walkdir::WalkDir;

fn is_binary(file_path: &Path) -> Result<bool> {
    let mut buffer = [0; 1024];
    let mut reader = BufReader::new(File::open(file_path)?);
    let mut total_read = 0;

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        if buffer.iter().take(read).any(|&byte| byte == 0) {
            return Ok(true);
        }
        total_read += read;
        if total_read >= 1024 {
            break;
        }
    }
    Ok(false)
}

fn process_file_contents(file_path: &Path, content: &str) -> String {
    let extension = file_path
        .extension()
        .and_then(|os_str| os_str.to_str())
        .unwrap_or("");

    let significant_whitespace_extensions = [
        "py", "nim", "hs", "yml", "yaml", "coffee", "jade", "pug", "slim", "sass", "haml",
    ];

    let no_indentation_extensions = [
        "rs", "js", "jsx", "ts", "tsx", "c", "cpp", "h", "hpp", "java", "go", "cs", "rb", "php",
        "swift", "kt", "kts", "scala", "groovy", "fs", "fsx", "clj", "cljs", "edn", "lisp", "el",
        "scm", "ss", "rkt", "jl", "lua", "tcl", "pl", "pm", "elm", "erl", "hrl", "v", "sv", "svh",
        "html", "css", "scss", "less", "json", "xml", "sql", "md", "toml", "ini", "conf", "cfg",
        "sh", "bash", "zsh", "ps1", "awk", "sed",
    ];
    let mut processed = String::new();
    for line in content.lines() {
        let processed_line = if significant_whitespace_extensions.contains(&extension) {
            line.replace("    ", "\t")
        } else if no_indentation_extensions.contains(&extension) {
            line.trim_start().to_string()
        } else {
            line.to_string()
        };

        if !processed_line.is_empty() {
            processed.push_str(&processed_line);
            processed.push('\n');
        }
    }
    processed
}

fn is_child_of(child: &str, parent: &str) -> bool {
    let parent = parent.trim_end_matches('/');
    child.starts_with(parent)
        && (child.len() == parent.len() || child[parent.len()..].starts_with('/'))
}

fn main() -> Result<()> {
    let matches = Command::new("gprepo")
        .version("0.1.0")
        .arg(
            Arg::new("output")
                .short('o')
                .long("output")
                .value_name("OUTPUT_PATH")
                .help("Output to path (default: stdout)")
                .required(false),
        )
        .arg(
            Arg::new("repo_path")
                .short('r')
                .long("repo-path")
                .value_name("REPO_PATH")
                .help("Path to the repository")
                .required(false),
        )
        .arg(
            Arg::new("preamble")
                .short('p')
                .long("preamble")
                .value_name("PREAMBLE_PATH")
                .help("Optional path to the preamble file")
                .required(false),
        )
        .arg(
            Arg::new("exclude")
                .short('e')
                .long("exclude")
                .value_name("EXCLUDE_PATH")
                .help("File paths to exclude (supports glob patterns)")
                .required(false)
                .num_args(1..)
                .action(clap::ArgAction::Append),
        )
        .arg(
            Arg::new("include")
                .short('i')
                .long("include")
                .value_name("INCLUDE_PATH")
                .help("Only process these specific paths (supports glob patterns)")
                .required(false)
                .num_args(1..)
                .action(clap::ArgAction::Append),
        )
        .get_matches();

    let output_path: Option<PathBuf> = matches.get_one::<String>("output").map(PathBuf::from);
    let process_start_time = SystemTime::now();

    let repo = match matches.get_one::<String>("repo_path") {
        Some(path) => Repository::discover(path).context("Could not find repository")?,
        None => {
            let current_dir = std::env::current_dir()?;
            Repository::discover(current_dir).context("Could not find repository")?
        }
    };

    let repo_path = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("Could not find repository working directory"))?;

    let mut _gitignore = repo
        .statuses(Some(
            StatusOptions::new()
                .include_ignored(false)
                .show(StatusShow::IndexAndWorkdir),
        ))
        .context("Failed to read gitignore")?;

    let exclude_set = {
        let mut builder = GlobSetBuilder::new();
        if let Some(exclude_paths) = matches.get_many::<String>("exclude") {
            for path in exclude_paths {
                builder.add(path.parse().unwrap());
            }
        }
        // Add default patterns
        builder.add("*changelog*".parse().unwrap());
        builder.add("*CHANGELOG*".parse().unwrap());
        builder.add(".github*".parse().unwrap());
        builder.add(".gitignore".parse().unwrap());
        builder.add("gprepo".parse().unwrap());
        builder.add("*LICENSE*".parse().unwrap());
        builder.add("*.lock".parse().unwrap());
        builder.add("*README*".parse().unwrap());
        builder.build().unwrap()
    };

    let mut writer: Box<dyn Write> = match matches.get_one::<String>("output") {
        Some(output_path) => Box::new(BufWriter::new(File::create(output_path)?)),
        None => Box::new(BufWriter::new(stdout())),
    };

    if let Some(preamble_path) = matches.get_one::<String>("preamble") {
        let mut preamble = String::new();
        File::open(preamble_path)?.read_to_string(&mut preamble)?;
        writeln!(writer, "{}", preamble)?;
    } else {
        writeln!(
            writer,
            "Below is a repository containing files. Each file begins with @@@@<file-path>@@@@ followed by its content. The repository ends with @@@@END@@@@. After this marker, instructions related to the repository are provided."
        )?;
    }

    for entry in WalkDir::new(repo_path) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let file_path = entry.path();
            let relative_file_path = file_path.strip_prefix(repo_path).unwrap();
            let path_str = relative_file_path.to_str().unwrap_or("");

            let mut should_exclude = false;
            if let Some(exclude_paths) = matches.get_many::<String>("exclude") {
                for exclude_path in exclude_paths {
                    if is_child_of(path_str, exclude_path) {
                        should_exclude = true;
                        break;
                    }
                }
            }

            let mut should_include = matches.get_many::<String>("include").is_none();
            if let Some(include_path) = matches.get_many::<String>("include") {
                for include_path in include_path {
                    if is_child_of(path_str, include_path) {
                        should_include = true;
                        break;
                    }
                }
            }

            if should_exclude || !should_include {
                continue;
            }

            if exclude_set.is_match(path_str) {
                continue;
            }

            let should_ignore = repo.status_should_ignore(relative_file_path).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to check if path should be ignored: {:?}", e),
                )
            })?;
            if should_ignore {
                continue;
            }

            if output_path
                .as_ref()
                .is_some_and(|op| op.as_path() == file_path)
            {
                continue;
            }

            let metadata = file_path.metadata()?;
            if let Ok(modified_time) = metadata.modified() {
                if modified_time >= process_start_time {
                    continue;
                }
            }

            if is_binary(file_path)? {
                continue;
            }

            writeln!(writer, "@@@@{}@@@@", relative_file_path.display())?;
            let mut file_contents = String::new();
            File::open(file_path)?.read_to_string(&mut file_contents)?;
            let processed_contents = process_file_contents(file_path, &file_contents);
            writeln!(writer, "{}", processed_contents)?;
        }
    }
    writeln!(writer, "@@@@END@@@@")?;
    Ok(())
}
