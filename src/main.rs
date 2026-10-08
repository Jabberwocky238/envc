use std::process::ExitCode;

use clap::{Parser, Subcommand};
use envc::{Config, Ctx, Env, Result, Shell};

mod cmd {
    pub mod autoload;
    pub mod create;
    pub mod delete;
    pub mod disable;
    pub mod enable;
    pub mod init;
    pub mod list;
    pub mod stack;
    pub mod status;
    pub mod unuse;
    pub mod r#use;
}

#[derive(Parser, Debug)]
#[command(
    name = "envc",
    version,
    about = "Profile based environment variable manager for bash",
    after_help = include_str!("text/after_help.txt"),
    disable_help_subcommand = true,
    arg_required_else_help = true
)]
struct Cli {
    #[arg(short, long, global = true, help = "Suppress informational messages")]
    quiet: bool,

    #[arg(
        long,
        global = true,
        value_name = "SHELL",
        help = "Which shell syntax to emit: bash, powershell or cmd"
    )]
    shell: Option<String>,

    #[arg(long, global = true, hide = true)]
    wrapped: bool,

    #[command(subcommand)]
    command: Command,
}

macro_rules! commands {
    ($($(#[$meta:meta])* $variant:ident($($module:ident)::+)),+ $(,)?) => {
        #[derive(Subcommand, Debug)]
        enum Command {
            $($(#[$meta])* $variant(cmd::$($module)::+::$variant),)+
        }

        impl Command {
            fn run(&self, ctx: &Ctx) -> Result<()> {
                match self {
                    $(Command::$variant(c) => c.run(ctx),)+
                }
            }
        }
    };
}

commands! {
    #[command(about = "Create a profile template at ~/.envc/profiles/<name>/.env")]
    Create(create),
    #[command(visible_alias = "ls", about = "List profiles, marking the startup one")]
    List(list),
    #[command(visible_alias = "rm", about = "Delete a profile")]
    Delete(delete),
    #[command(visible_alias = "push", about = "Push a profile onto this shell's stack")]
    Use(r#use),
    #[command(visible_alias = "pop", about = "Take a profile off this shell's stack, restoring what it covered")]
    Unuse(unuse),
    #[command(about = "Set up shell integration: detect the login shell's rc file and inject the startup hook into it (safe to re-run)")]
    Init(init),
    #[command(about = "Choose the profile new shells load and install the startup hook")]
    Enable(enable),
    #[command(about = "Stop new shells from loading the startup profile (this shell is left alone)")]
    Disable(disable),
    #[command(about = "Show the restore stack")]
    Stack(stack),
    #[command(about = "Show what is enabled and in use right now")]
    Status(status),
    #[command(hide = true, about = "Internal: emit shell code for the rc hook")]
    Autoload(autoload),
}

impl Cli {
    fn run(self) -> Result<()> {
        let env = Env::read();
        let config = Config::load(&env)?;
        let ctx = Ctx {
            shell: Shell::resolve(self.shell.as_deref(), &config, &env)?,
            quiet: self.quiet,
            wrapped: self.wrapped,
            config,
            env,
        };
        self.command.run(&ctx)
    }
}

fn main() -> ExitCode {
    match Cli::parse().run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("envc: {e:#}");
            ExitCode::FAILURE
        }
    }
}
