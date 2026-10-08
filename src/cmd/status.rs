use clap::Args;
use envc::{Ctx, Loader, Result, Stack, StateFile, Startup};

#[derive(Args, Debug)]
pub struct Status;

impl Status {
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        let env = &ctx.env;
        let c = ctx.palette();
        let platform = ctx.platform();
        let row = |label: &str, value: String| println!("  {label:<17}{value}");

        println!("{} {}", c.bold("envc"), env!("CARGO_PKG_VERSION"));
        row("home", env.tilde(&env.envc_home()?).to_string());
        row("shell", ctx.shell.to_string());

        let enabled = platform.is_on()?;
        let state = if enabled { c.green("enabled") } else { c.yellow("disabled") };
        row("startup loading", format!("{state} ({})", platform.describe()?));

        let startup = Startup::current(env)?;
        row(
            "startup profile",
            match (enabled, &startup) {
                (true, Some(s)) => c.bold(s),
                (true, None) => c.dim("(none -- `envc enable <name>`)"),
                (false, Some(s)) => format!("{} {}", c.bold(s), c.dim("(selected, loading off)")),
                (false, None) => c.dim("(none)"),
            },
        );

        let active = env.active.as_deref();
        row("this shell", active.map_or_else(|| c.dim("(none)"), |a| c.bold(a)));

        row(
            "restore stack",
            match Stack::load(env)? {
                Some(s) => format!("{} ({} vars, {})", s.chain(), s.var_count(), env.tilde(&Stack::path(env)?)),
                None => c.dim("(empty)"),
            },
        );

        if let (true, Some(startup), Some(cur)) = (enabled, &startup, active) {
            if startup != cur {
                eprintln!();
                eprintln!("envc: note: this shell has '{cur}', new shells load '{startup}'.");
                eprintln!("envc:       `envc enable <name>` changes what new shells load.");
            }
        }

        let profiles = env.profiles_dir()?;
        row(
            "profiles",
            format!("{} in {}", env.profile_names()?.len(), env.tilde(&profiles)),
        );
        Ok(())
    }
}
