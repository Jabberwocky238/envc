use anyhow::{anyhow, Context as _};
use clap::Args;
use envc::{Ctx, ProfileName, Result, Stack, StateFile, Startup};

#[derive(Args, Debug)]
pub struct Delete {
    #[arg(help = "Profile to delete")]
    name: String,

    #[arg(short, long, help = "Delete even when the profile is in use or loads at startup")]
    force: bool,
}

impl Delete {
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        let ProfileName(name) = self.name.as_str().try_into()?;
        let env = &ctx.env;
        let dir = env.profile_dir(name)?;
        if !dir.is_dir() {
            return Err(anyhow!("no such profile: {name}"));
        }

        let in_use = Stack::load(env)?.is_some_and(|s| s.position(name).is_some());
        let is_startup = Startup::current(env)?.as_deref() == Some(name);

        if (in_use || is_startup) && !self.force {
            let why = match in_use {
                true => format!("in use; run `envc unuse {name}` first"),
                false => format!("the startup profile; run `envc disable {name}` first"),
            };
            return Err(anyhow!("profile '{name}' is {why}, or pass --force"));
        }

        std::fs::remove_dir_all(&dir).with_context(|| format!("cannot remove {}", env.tilde(&dir)))?;

        if is_startup {
            Startup::remove(env)?;
            ctx.notify(format!("cleared the startup profile ('{name}' deleted)"));
            ctx.notify("warning: new shells load nothing until you pick one: envc enable <name>");
        }
        if in_use {
            ctx.notify(format!(
                "warning: '{name}' is still on the restore stack; `envc unuse {name}` takes it off"
            ));
        }

        println!("deleted profile '{name}' ({})", env.tilde(&dir));
        Ok(())
    }
}
