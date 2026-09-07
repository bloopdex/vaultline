//! `vaultline timer generate` / `vaultline timer install` — the systemd
//! story (the design philosophy's "boring technology: single static
//! binary + config + systemd timer").
//!
//! One service per job plus one timer per declared schedule:
//! - `vaultline-<app>.service` runs `backup run` (when
//!   `application.schedule` is declared), started by
//!   `vaultline-<app>.timer` with the cron translated to `OnCalendar`,
//! - `vaultline-<app>-verify.service` runs `backup verify latest` (when
//!   `verification.schedule` is declared), started by its own timer.
//!
//! Both timers are `Persistent=true` (systemd catches up a run missed
//! while the host was down). `generate` writes the unit files anywhere
//! (any platform); `install` writes them into `/etc/systemd/system`,
//! daemon-reloads, and enables the timers — Linux only, and it never
//! starts them (starting is an explicit `--now`).
//!
//! A definition with no schedules at all generates nothing and errors:
//! there is nothing to schedule.

use std::path::{Path, PathBuf};

use vaultline_core::config;
use vaultline_core::error::{ErrorKind, Result, VaultlineError};

#[derive(clap::Args)]
pub struct TimerArgs {
    #[command(subcommand)]
    pub command: TimerCommand,
}

#[derive(clap::Subcommand)]
pub enum TimerCommand {
    /// Write the systemd unit files to a directory (default:
    /// ./vaultline-systemd). Regeneration overwrites the files — they are
    /// derived artifacts.
    Generate(TimerGenerateArgs),
    /// Install the units into /etc/systemd/system, daemon-reload, and
    /// enable the timers (Linux only; use --now to also start them).
    Install(TimerInstallArgs),
}

#[derive(clap::Args)]
pub struct TimerGenerateArgs {
    #[arg(long, default_value = "vaultline.toml")]
    pub config: PathBuf,

    /// The directory the unit files are written to.
    #[arg(long, default_value = "vaultline-systemd")]
    pub out: PathBuf,
}

#[derive(clap::Args)]
pub struct TimerInstallArgs {
    #[arg(long, default_value = "vaultline.toml")]
    pub config: PathBuf,

    /// Also start the timers (systemctl enable --now).
    #[arg(long)]
    pub now: bool,
}

/// The absolute path of the configuration file as the generated unit will
/// see it (the unit runs from the host's root context).
fn absolute_config(path: &Path) -> Result<String> {
    if path.is_absolute() {
        Ok(path.to_string_lossy().into_owned())
    } else {
        let cwd = std::env::current_dir().map_err(|e| {
            VaultlineError::with_source(ErrorKind::Io, "cannot resolve the current directory", e)
        })?;
        Ok(cwd.join(path).to_string_lossy().into_owned())
    }
}

/// The vaultline binary the units will invoke — this generator itself.
fn current_binary() -> Result<String> {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Operational,
                "cannot determine the vaultline binary path for the unit files",
                e,
            )
        })
}

/// systemd unit-file escaping: `%` introduces specifiers, so literal
/// percents are doubled. The only character systemd units require this
/// for.
fn escape_unit(value: &str) -> String {
    value.replace('%', "%%")
}

/// Build the unit pair for one job: (service contents, timer contents,
/// timer unit name). `None` for both when the schedule is absent.
fn build_units(
    app: &vaultline_core::model::Application,
    binary: &str,
    config_path: &str,
    job: Job,
) -> Result<Option<(String, String, String)>> {
    let (schedule, description, command_args, suffix) = match job {
        Job::Backup => (
            app.schedule.as_deref(),
            "vaultline backup",
            "backup run",
            "",
        ),
        Job::Verify => (
            app.verification.schedule.as_deref(),
            "vaultline verification",
            "backup verify latest",
            "-verify",
        ),
    };
    let Some(schedule) = schedule else {
        return Ok(None);
    };
    let on_calendar = vaultline_core::cron::to_on_calendar(schedule).map_err(|message| {
        VaultlineError::new(
            ErrorKind::Config,
            format!("the schedule \"{schedule}\" cannot be expressed as a systemd OnCalendar value: {message}"),
        )
    })?;

    let unit = format!("vaultline-{}{suffix}", app.name);
    let service = format!(
        "[Unit]\nDescription={description} for {name}\n\n[Service]\nType=oneshot\nExecStart={binary} {command_args} --config {config_path}\n",
        name = app.name,
    );
    let timer = format!(
        "[Unit]\nDescription={description} timer for {name}\n\n[Timer]\nOnCalendar={on_calendar}\nPersistent=true\nUnit={unit}.service\n\n[Install]\nWantedBy=timers.target\n",
        name = app.name,
    );
    Ok(Some((service, timer, unit)))
}

#[derive(Clone, Copy)]
enum Job {
    Backup,
    Verify,
}

pub fn run_timer(args: TimerArgs) -> Result<()> {
    match args.command {
        TimerCommand::Generate(args) => run_generate(args),
        TimerCommand::Install(args) => run_install(args),
    }
}

fn run_generate(args: TimerGenerateArgs) -> Result<()> {
    let app = config::load(&args.config)?;
    let binary = escape_unit(&current_binary()?);
    let config_path = escape_unit(&absolute_config(&args.config)?);

    let mut units: Vec<(String, String, String)> = Vec::new();
    for job in [Job::Backup, Job::Verify] {
        if let Some((service, timer, unit_name)) = build_units(&app, &binary, &config_path, job)? {
            units.push((service, timer, unit_name));
        }
    }
    if units.is_empty() {
        return Err(VaultlineError::new(
            ErrorKind::Config,
            "no schedules declared: set application.schedule or application.verification.schedule — nothing to schedule",
        ));
    }

    std::fs::create_dir_all(&args.out).map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            format!("cannot create {}", args.out.display()),
            e,
        )
    })?;
    for (service, timer, unit_name) in &units {
        let service_path = args.out.join(format!("{unit_name}.service"));
        let timer_path = args.out.join(format!("{unit_name}.timer"));
        std::fs::write(&service_path, service).map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Io,
                format!("cannot write {}", service_path.display()),
                e,
            )
        })?;
        std::fs::write(&timer_path, timer).map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Io,
                format!("cannot write {}", timer_path.display()),
                e,
            )
        })?;
        println!(
            "wrote {} + {}",
            service_path.display(),
            timer_path.display()
        );
    }
    Ok(())
}

fn run_install(args: TimerInstallArgs) -> Result<()> {
    if std::env::consts::OS != "linux" {
        return Err(VaultlineError::new(
            ErrorKind::Unsupported,
            "timer install is Linux-only (systemd); use `vaultline timer generate` to write unit files, or run `vaultline schedule run` from another scheduler on this platform",
        ));
    }

    let app = config::load(&args.config)?;
    let binary = escape_unit(&current_binary()?);
    let config_path = escape_unit(&absolute_config(&args.config)?);

    let system_dir = Path::new("/etc/systemd/system");
    let mut timer_names: Vec<String> = Vec::new();
    for job in [Job::Backup, Job::Verify] {
        let Some((service, timer, unit_name)) = build_units(&app, &binary, &config_path, job)?
        else {
            continue;
        };
        let service_path = system_dir.join(format!("{unit_name}.service"));
        let timer_path = system_dir.join(format!("{unit_name}.timer"));
        std::fs::write(&service_path, service).map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Io,
                format!(
                    "cannot write {} (root privileges required)",
                    service_path.display()
                ),
                e,
            )
        })?;
        std::fs::write(&timer_path, timer).map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Io,
                format!("cannot write {}", timer_path.display()),
                e,
            )
        })?;
        println!(
            "installed {} + {}",
            service_path.display(),
            timer_path.display()
        );
        timer_names.push(format!("{unit_name}.timer"));
    }
    if timer_names.is_empty() {
        return Err(VaultlineError::new(
            ErrorKind::Config,
            "no schedules declared: set application.schedule or application.verification.schedule — nothing to schedule",
        ));
    }

    // daemon-reload so systemd sees the new units, then enable (start only
    // on --now: safe by default).
    let status = std::process::Command::new("systemctl")
        .arg("daemon-reload")
        .status()
        .map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Operational,
                "cannot run systemctl daemon-reload (is systemd running?)",
                e,
            )
        })?;
    if !status.success() {
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!("systemctl daemon-reload failed (exit {status})"),
        ));
    }
    for timer_name in &timer_names {
        let mut command = std::process::Command::new("systemctl");
        command.arg("enable");
        if args.now {
            command.arg("--now");
        }
        command.arg(timer_name);
        let status = command.status().map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Operational,
                format!("cannot run systemctl enable for {timer_name}"),
                e,
            )
        })?;
        if !status.success() {
            return Err(VaultlineError::new(
                ErrorKind::Operational,
                format!("systemctl enable {timer_name} failed (exit {status})"),
            ));
        }
        println!(
            "enabled {timer_name}{}",
            if args.now { " (started)" } else { "" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vaultline_core::model::{
        Application, RestoreProcedure, RetentionPolicy, StorageKind, StorageTarget,
        VerificationLevel, VerificationPolicy,
    };

    fn app_with(schedule: Option<&str>, verify_schedule: Option<&str>) -> Application {
        Application {
            name: "thornwa".to_string(),
            description: None,
            schedule: schedule.map(String::from),
            sources: Vec::new(),
            databases: Vec::new(),
            volumes: Vec::new(),
            storage: StorageTarget {
                kind: StorageKind::Local {
                    path: "/var/backups".to_string(),
                },
                repository: None,
                password_env: "PW".to_string(),
            },
            retention: RetentionPolicy {
                keep_last: 7,
                keep_daily: 0,
                keep_weekly: 0,
                keep_monthly: 0,
                keep_yearly: 0,
            },
            verification: VerificationPolicy {
                level: VerificationLevel::L1,
                schedule: verify_schedule.map(String::from),
                app_checks: Vec::new(),
            },
            restore: RestoreProcedure { steps: Vec::new() },
        }
    }

    #[test]
    fn backup_unit_contains_the_translated_schedule() {
        let app = app_with(Some("30 2 * * *"), None);
        let (service, timer, unit) = build_units(
            &app,
            "/usr/bin/vaultline",
            "/etc/vaultline/thornwa.toml",
            Job::Backup,
        )
        .expect("built")
        .expect("some");
        assert_eq!(unit, "vaultline-thornwa");
        assert!(service.contains(
            "ExecStart=/usr/bin/vaultline backup run --config /etc/vaultline/thornwa.toml"
        ));
        assert!(service.contains("[Service]\nType=oneshot"));
        assert!(timer.contains("OnCalendar=*-*-* 2:30:00"));
        assert!(timer.contains("Persistent=true"));
        assert!(timer.contains("Unit=vaultline-thornwa.service"));
        assert!(timer.contains("WantedBy=timers.target"));
    }

    #[test]
    fn verify_unit_uses_latest_and_its_suffix() {
        let app = app_with(None, Some("0 9 * * 2-6"));
        let (service, timer, unit) = build_units(
            &app,
            "/usr/bin/vaultline",
            "/etc/vaultline/thornwa.toml",
            Job::Verify,
        )
        .expect("built")
        .expect("some");
        assert_eq!(unit, "vaultline-thornwa-verify");
        assert!(service.contains("backup verify latest"));
        assert!(timer.contains("OnCalendar=Mon..Fri *-*-* 9:0:00"));
    }

    #[test]
    fn absent_schedule_produces_no_unit() {
        let app = app_with(None, None);
        assert!(
            build_units(&app, "/bin/v", "/cfg", Job::Backup)
                .expect("ok")
                .is_none()
        );
        assert!(
            build_units(&app, "/bin/v", "/cfg", Job::Verify)
                .expect("ok")
                .is_none()
        );
    }

    #[test]
    fn percents_are_escaped_for_unit_files() {
        assert_eq!(escape_unit("50%"), "50%%");
        assert_eq!(escape_unit("/plain/path"), "/plain/path");
    }

    #[test]
    fn untranslatable_schedule_errors() {
        // A schedule the cron crate accepts but OnCalendar cannot express:
        // a step over a single day value.
        let app = app_with(Some("0 3 1/2 * *"), None);
        let err = build_units(&app, "/bin/v", "/cfg", Job::Backup).expect_err("untranslatable");
        assert!(err.to_string().contains("OnCalendar"), "{err}");
        assert_eq!(err.exit_code(), 2);
    }
}
