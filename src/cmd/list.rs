use clap::Args;
use envc::{Assignment, Ctx, Result, Startup};

#[derive(Args, Debug)]
pub struct List;

impl List {
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        let env = &ctx.env;
        let dir = env.profiles_dir()?;
        let startup = Startup::current(env)?;
        let c = ctx.palette();
        let names = env.profile_names()?;

        if names.is_empty() {
            println!("no profiles yet.");
            println!("create one with: envc create <name>");
            println!();
            return ctx.startup_line(&c, startup.as_deref());
        }

        let mut rows = Vec::new();
        for name in &names {
            let file = env.profile_env_file(name)?;
            let (count, error) = match Assignment::load(env, &file, &env.vars) {
                Ok(a) => (a.len().to_string(), None),
                Err(e) => ("!".to_string(), Some(e.to_string())),
            };
            rows.push((name, count, error, file));
        }
        let width = names.iter().map(|n| n.chars().count()).max().unwrap_or(0).max("PROFILE".len());

        println!("  {:<width$}  {:>4}  {}", c.bold("PROFILE"), "VARS", c.bold("ENV FILE"));
        for (name, count, error, file) in &rows {
            let is_startup = startup.as_deref() == Some(name.as_str());
            let marker = if is_startup { c.green("*") } else { " ".to_string() };
            let label = format!("{name:<width$}");
            let label = if is_startup { c.bold(&label) } else { label };
            let file = env.tilde(file);
            let file = if error.is_some() { c.red(file) } else { c.dim(file) };
            println!("{marker} {label}  {count:>4}  {file}");
        }

        for (name, _, error, _) in &rows {
            if let Some(e) = error {
                eprintln!("envc: profile '{name}' could not be parsed: {e}");
            }
        }
        if let Some(startup) = startup.as_ref().filter(|s| !names.contains(s)) {
            eprintln!(
                "envc: startup profile '{startup}' has no directory under {}",
                env.tilde(&dir)
            );
        }

        println!();
        ctx.startup_line(&c, startup.as_deref())
    }
}
