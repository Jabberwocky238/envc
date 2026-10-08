use anyhow::{anyhow, Context as _};
use clap::Args;
use envc::{Ctx, Loader, ProfileName, Result};

#[derive(Args, Debug)]
pub struct Create {
    #[arg(help = "Profile name (letters, digits, '_', '-', '.')")]
    name: String,

    #[arg(short, long, help = "Overwrite the profile if it already exists")]
    force: bool,
}

impl Create {
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        let ProfileName(name) = self.name.as_str().try_into()?;
        let env = &ctx.env;
        let dir = env.profile_dir(name)?;
        let file = env.profile_env_file(name)?;

        if file.exists() && !self.force {
            return Err(anyhow!(
                "profile '{name}' already exists ({}); pass --force to overwrite it",
                env.tilde(&file)
            ));
        }

        std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", env.tilde(&dir)))?;
        std::fs::write(&file, format!("# envc profile: {name}\n"))
            .with_context(|| format!("cannot write {}", env.tilde(&file)))?;

        println!("created profile '{name}'");
        println!("  env file: {}", env.tilde(&file));
        println!("  edit it, then run: envc use {name}");

        if !ctx.platform().is_on()? {
            println!();
            println!("note: startup loading is off, so new shells will not load this profile.");
            println!("      run `envc enable {name}` to turn it on.");
        }
        Ok(())
    }
}
