use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::core::config::{Config, RemoteProtocol};
use crate::core::paths;
use crate::core::state::{Event, Snapshot};
use crate::integrations::statefiles::{self, StatusFile};
use crate::ipc::{self, Request, Response};

#[derive(Parser)]
#[command(
    name = "duskr",
    version,
    about = "System-wide speech-to-text for Linux"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Daemon,
    Toggle(LanguageArg),
    Start(LanguageArg),
    Stop,
    Cancel,
    Pause,
    Resume,
    Submit,
    Reload,
    Shutdown,
    Status {
        #[arg(long)]
        json: bool,
    },
    Transcribe {
        path: PathBuf,
    },
    Backend {
        #[command(subcommand)]
        command: BackendCommand,
    },
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
    Quickshell {
        #[command(subcommand)]
        command: QuickshellCommand,
    },
    Waybar(WatchArg),
    Integration {
        #[command(subcommand)]
        command: IntegrationCommand,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    Doctor,
}

#[derive(Args)]
struct LanguageArg {
    #[arg(long)]
    language: Option<String>,
}

#[derive(Args)]
struct WatchArg {
    #[arg(long)]
    watch: bool,
}

#[derive(Subcommand)]
enum BackendCommand {
    List,
    Set { id: String },
}

#[derive(Subcommand)]
enum ModelCommand {
    Directory {
        path: Option<PathBuf>,
        #[arg(long, requires = "path")]
        move_models: bool,
    },
    List,
    Catalog,
    Add {
        path: PathBuf,
        #[arg(long)]
        move_file: bool,
    },
    Remove {
        name: String,
    },
    Download {
        name: String,
        #[arg(long)]
        force: bool,
    },
    Unload,
    Reload,
}

#[derive(Subcommand)]
enum ConfigCommand {
    Path,
    Show,
    Init {
        #[arg(long)]
        force: bool,
    },
    SetModel {
        model: String,
    },
    SetBackend {
        backend: String,
    },
    SetRemote(RemoteArgs),
}

#[derive(Args)]
struct RemoteArgs {
    url: String,
    #[arg(long, default_value = "auto")]
    protocol: ProtocolArg,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    api_key_file: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum ProtocolArg {
    Auto,
    Pcm,
    Multipart,
}

#[derive(Subcommand)]
enum MigrateCommand {
    Hyprwhspr {
        #[arg(long)]
        move_models: bool,
        #[arg(long)]
        skip_quickshell: bool,
        #[arg(long)]
        no_start: bool,
        #[arg(long)]
        keep_hyprwhspr: bool,
    },
}

#[derive(Subcommand)]
enum QuickshellCommand {
    Status,
    AudioLevel,
    Watch,
    Install,
}

#[derive(Subcommand)]
enum IntegrationCommand {
    Waybar,
    Hyprland,
    Kde,
    Quickshell,
}

#[derive(Subcommand)]
enum ServiceCommand {
    Install,
    Start,
    Stop,
    Restart,
    Status,
    Enable,
    Disable,
}

pub async fn run() -> Result<()> {
    run_with(Cli::parse()).await
}

async fn run_with(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Daemon => crate::daemon::run().await,
        Command::Toggle(arg) => {
            call(Request::Toggle {
                language: arg.language,
            })
            .await
        }
        Command::Start(arg) => {
            call(Request::Start {
                language: arg.language,
            })
            .await
        }
        Command::Stop => call(Request::Stop).await,
        Command::Cancel => call(Request::Cancel).await,
        Command::Pause => call(Request::Pause).await,
        Command::Resume => call(Request::Resume).await,
        Command::Submit => call(Request::Submit).await,
        Command::Reload => call(Request::Reload).await,
        Command::Shutdown => call(Request::Shutdown).await,
        Command::Status { json } => status(json).await,
        Command::Transcribe { path } => {
            call(Request::TranscribeFile {
                path: path.to_string_lossy().into_owned(),
            })
            .await
        }
        Command::Backend { command } => backend(command).await,
        Command::Model { command } => model(command).await,
        Command::Config { command } => config(command).await,
        Command::Migrate { command } => migrate(command).await,
        Command::Quickshell { command } => quickshell(command).await,
        Command::Waybar(arg) => shell_stream(arg.watch, true).await,
        Command::Integration { command } => integration(command),
        Command::Service { command } => service(command).await,
        Command::Doctor => doctor().await,
    }
}

async fn call(request: Request) -> Result<()> {
    print_response(ipc::client::request(request).await?)
}

fn print_response(response: Response) -> Result<()> {
    match response {
        Response::Ok => Ok(()),
        Response::Text { text } => {
            println!("{text}");
            Ok(())
        }
        Response::Status(snapshot) => {
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
            Ok(())
        }
        Response::Error { message } => bail!(message),
        Response::Event(_) => Ok(()),
    }
}

async fn status(json: bool) -> Result<()> {
    let mut client = ipc::client::Client::connect().await?;
    let snapshot = client.status().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    } else {
        println!("{}", snapshot.tooltip());
    }
    Ok(())
}

async fn backend(command: BackendCommand) -> Result<()> {
    match command {
        BackendCommand::List => {
            let configured = Config::load_or_default().asr.backend;
            for (id, description) in crate::asr::KNOWN_BACKENDS {
                let marker = if crate::asr::canonical_backend_id(&configured) == *id {
                    "*"
                } else {
                    " "
                };
                println!("{marker} {id:<8} {description}");
            }
            Ok(())
        }
        BackendCommand::Set { id } => set_backend(&id).await,
    }
}

async fn model(command: ModelCommand) -> Result<()> {
    let mut config = Config::load_or_default();
    match command {
        ModelCommand::Directory { path: None, .. } => {
            println!("{}", config.models_dir().display());
            Ok(())
        }
        ModelCommand::Directory {
            path: Some(path),
            move_models,
        } => {
            let report = crate::models::relocate(&mut config, &path, move_models)?;
            config.save()?;
            println!("{}", report.to.display());
            if !report.moved.is_empty() {
                println!("moved: {}", report.moved.join(", "));
            }
            notify_reload().await
        }
        ModelCommand::List => {
            let entries = crate::models::list(&config.models_dir());
            if entries.is_empty() {
                println!("no models in {}", config.models_dir().display());
            }
            for entry in entries {
                println!(
                    "{:<24} {:>10}  {}",
                    entry.name,
                    entry.size_human(),
                    entry.path.display()
                );
            }
            Ok(())
        }
        ModelCommand::Catalog => {
            for (name, description) in crate::models::CATALOG {
                println!("{name:<20} {description}");
            }
            Ok(())
        }
        ModelCommand::Add { path, move_file } => {
            let entry = crate::models::add(&config.models_dir(), &path, move_file)?;
            println!("added {} at {}", entry.name, entry.path.display());
            Ok(())
        }
        ModelCommand::Remove { name } => {
            let removed = crate::models::remove(&config.models_dir(), &name)?;
            println!("removed {}", removed.display());
            Ok(())
        }
        ModelCommand::Download { name, force } => {
            if !crate::models::is_catalog_model(&name) {
                bail!("unknown model '{name}'; run `duskr model catalog`");
            }
            let mut last_percent = u64::MAX;
            let path = crate::models::download::download(
                &config.models.download_base_url,
                &name,
                &config.models_dir(),
                force,
                |progress| {
                    if let Some(fraction) = progress.fraction() {
                        let percent = (fraction * 100.0) as u64;
                        if percent != last_percent {
                            eprint!("\r{percent:3}%");
                            last_percent = percent;
                        }
                    }
                },
            )
            .await?;
            if last_percent != u64::MAX {
                eprintln!();
            }
            println!("{}", path.display());
            Ok(())
        }
        ModelCommand::Unload => call(Request::ModelUnload).await,
        ModelCommand::Reload => call(Request::ModelReload).await,
    }
}

async fn config(command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Path => {
            println!("{}", paths::config_file().display());
            Ok(())
        }
        ConfigCommand::Show => {
            let config = Config::load_or_default();
            print!("{}", toml::to_string_pretty(&config)?);
            Ok(())
        }
        ConfigCommand::Init { force } => {
            let path = paths::config_file();
            if path.exists() && !force {
                bail!(
                    "{} already exists; use --force to replace it",
                    path.display()
                );
            }
            if path.exists() {
                crate::migrate::backup(&path)?;
            }
            Config::default().save()?;
            println!("{}", path.display());
            Ok(())
        }
        ConfigCommand::SetModel { model } => set_model(&model).await,
        ConfigCommand::SetBackend { backend } => set_backend(&backend).await,
        ConfigCommand::SetRemote(args) => {
            let mut config = Config::load_or_default();
            config.asr.backend = "remote".into();
            config.asr.remote.url = args.url.trim_end_matches('/').to_string();
            config.asr.remote.protocol = match args.protocol {
                ProtocolArg::Auto => RemoteProtocol::Auto,
                ProtocolArg::Pcm => RemoteProtocol::Pcm,
                ProtocolArg::Multipart => RemoteProtocol::Multipart,
            };
            config.asr.remote.model = args.model;
            if let Some(path) = args.api_key_file {
                let key = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?
                    .trim()
                    .to_string();
                if key.is_empty() {
                    bail!("{} is empty", path.display());
                }
                config.asr.remote.api_key = Some(key);
            }
            config.save()?;
            notify_reload().await
        }
    }
}

async fn set_model(model: &str) -> Result<()> {
    let mut config = Config::load_or_default();
    config.asr.whisper.model = model.to_string();
    config.save()?;
    if ipc::client::is_running().await {
        print_response(ipc::client::request(Request::SetModel { name: model.into() }).await?)?;
    }
    Ok(())
}

async fn set_backend(id: &str) -> Result<()> {
    let canonical = crate::asr::canonical_backend_id(id);
    if canonical == "unknown" {
        bail!("unknown backend '{id}'");
    }
    let mut config = Config::load_or_default();
    config.asr.backend = canonical.into();
    config.save()?;
    if ipc::client::is_running().await {
        print_response(
            ipc::client::request(Request::SetBackend {
                id: canonical.into(),
            })
            .await?,
        )?;
    }
    Ok(())
}

async fn notify_reload() -> Result<()> {
    if ipc::client::is_running().await {
        print_response(ipc::client::request(Request::Reload).await?)?;
    }
    Ok(())
}

async fn migrate(command: MigrateCommand) -> Result<()> {
    match command {
        MigrateCommand::Hyprwhspr {
            move_models,
            skip_quickshell,
            no_start,
            keep_hyprwhspr,
        } => {
            let report = crate::migrate::run_hyprwhspr(crate::migrate::Options {
                move_models,
                migrate_quickshell: !skip_quickshell,
                start_daemon: !no_start,
                disable_hyprwhspr: !keep_hyprwhspr,
            })
            .await?;
            println!("config: {}", report.config.display());
            println!("models imported: {}", report.models.len());
            if let Some(path) = report.quickshell_service {
                println!("quickshell: {}", path.display());
            }
            for note in report.notes {
                println!("{note}");
            }
            Ok(())
        }
    }
}

async fn quickshell(command: QuickshellCommand) -> Result<()> {
    match command {
        QuickshellCommand::Status => shell_stream(false, false).await,
        QuickshellCommand::AudioLevel => {
            let mut client = ipc::client::Client::connect().await?;
            println!("{:.3}", client.status().await?.level);
            Ok(())
        }
        QuickshellCommand::Watch => shell_stream(true, false).await,
        QuickshellCommand::Install => {
            let report =
                crate::migrate::quickshell::install(&crate::migrate::quickshell::detect())?;
            println!("{}", report.service_written.display());
            Ok(())
        }
    }
}

async fn shell_stream(watch: bool, waybar: bool) -> Result<()> {
    let mut client = ipc::client::Client::connect().await?;
    let snapshot = client.status().await?;
    print_shell(&snapshot, waybar)?;
    if !watch {
        return Ok(());
    }
    client
        .subscribe(|event| {
            match event {
                Event::State(snapshot) => print_shell(&snapshot, waybar)?,
                Event::Level { level } => {
                    if !waybar {
                        println!("{}", serde_json::json!({ "level": level }));
                    }
                }
                Event::Transcript { text, final_: true } => {
                    if !waybar {
                        println!("{}", serde_json::json!({ "transcript": text }));
                    }
                }
                Event::Shutdown => return Ok(false),
                _ => {}
            }
            Ok(true)
        })
        .await
}

fn print_shell(snapshot: &Snapshot, waybar: bool) -> Result<()> {
    let mut status = statefiles::status_file(snapshot);
    status.transcript = snapshot.last_transcript.clone();
    if waybar {
        println!("{}", serde_json::to_string(&WaybarStatus::from(status))?);
    } else {
        println!("{}", serde_json::to_string(&status)?);
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct WaybarStatus {
    text: String,
    alt: String,
    tooltip: String,
    class: String,
}

impl From<StatusFile> for WaybarStatus {
    fn from(status: StatusFile) -> Self {
        Self {
            text: status.text,
            alt: status.alt,
            tooltip: status.tooltip,
            class: status.class,
        }
    }
}

fn integration(command: IntegrationCommand) -> Result<()> {
    match command {
        IntegrationCommand::Waybar => {
            print!(
                "{}\n{}",
                crate::integrations::desktop::waybar_module(),
                crate::integrations::desktop::waybar_style()
            );
        }
        IntegrationCommand::Hyprland => {
            print!(
                "{}",
                crate::integrations::desktop::hyprland_config(&Config::load_or_default())
            );
        }
        IntegrationCommand::Kde => print!("{}", crate::integrations::desktop::kde_desktop_entry()),
        IntegrationCommand::Quickshell => {
            print!("{}", crate::integrations::desktop::quickshell_service())
        }
    }
    Ok(())
}

async fn service(command: ServiceCommand) -> Result<()> {
    use crate::integrations::systemd;
    match command {
        ServiceCommand::Install => {
            let path = systemd::install()?;
            systemd::systemctl(&["daemon-reload"]).await?;
            println!("{}", path.display());
            Ok(())
        }
        ServiceCommand::Start => {
            print_systemctl(systemd::systemctl(&["start", systemd::UNIT_NAME]).await)
        }
        ServiceCommand::Stop => {
            print_systemctl(systemd::systemctl(&["stop", systemd::UNIT_NAME]).await)
        }
        ServiceCommand::Restart => {
            print_systemctl(systemd::systemctl(&["restart", systemd::UNIT_NAME]).await)
        }
        ServiceCommand::Status => {
            print_systemctl(systemd::systemctl(&["status", systemd::UNIT_NAME, "--no-pager"]).await)
        }
        ServiceCommand::Enable => {
            print_systemctl(systemd::systemctl(&["enable", "--now", systemd::UNIT_NAME]).await)
        }
        ServiceCommand::Disable => {
            print_systemctl(systemd::systemctl(&["disable", "--now", systemd::UNIT_NAME]).await)
        }
    }
}

fn print_systemctl(result: Result<String>) -> Result<()> {
    let output = result?;
    if !output.is_empty() {
        println!("{output}");
    }
    Ok(())
}

async fn doctor() -> Result<()> {
    let mut failed = false;
    let checks = [
        (
            "PipeWire socket",
            pipewire_socket().exists(),
            pipewire_socket().display().to_string(),
        ),
        (
            "/dev/uinput",
            crate::input::uinput::diagnose().is_ok(),
            crate::input::uinput::diagnose()
                .err()
                .unwrap_or_else(|| "ok".into()),
        ),
        (
            "configuration",
            Config::load().is_ok(),
            paths::config_file().display().to_string(),
        ),
        (
            "daemon",
            ipc::client::is_running().await,
            paths::socket_path().display().to_string(),
        ),
    ];
    for (name, ok, detail) in checks {
        println!("{} {name}: {detail}", if ok { "ok" } else { "fail" });
        failed |= !ok;
    }
    if failed {
        bail!("one or more checks failed");
    }
    Ok(())
}

fn pipewire_socket() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("pipewire-0")
}

pub async fn wait_for_daemon(timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if ipc::client::is_running().await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!(
        "Duskr daemon did not start within {} seconds",
        timeout.as_secs()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_commands_parse() {
        for args in [
            vec!["duskr", "toggle"],
            vec!["duskr", "start"],
            vec!["duskr", "model", "directory"],
            vec!["duskr", "model", "add", "/tmp/model.bin"],
            vec!["duskr", "config", "set-model", "medium.en"],
            vec!["duskr", "migrate", "hyprwhspr", "--no-start"],
            vec!["duskr", "quickshell", "audio-level"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
    }

    #[test]
    fn model_move_requires_a_destination() {
        assert!(Cli::try_parse_from(["duskr", "model", "directory", "--move-models"]).is_err());
    }
}
