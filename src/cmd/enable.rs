use clap::Args;
use envc::{Ctx, Loader, ProfileName, Result, StateFile, Startup};

#[derive(Args, Debug)]
pub struct Enable {
    #[arg(help = "Profile new shells load")]
    name: String,
}

impl Enable {
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        let ProfileName(name) = self.name.as_str().try_into()?;
        ctx.env.existing_profile(name)?;
        let startup = Startup {
            profile: name.to_string(),
        };
        let platform = ctx.platform();

        let what = platform.apply(&startup.profile)?;
        startup.save(&ctx.env)?;

        println!("startup loading enabled: {}", startup.profile);
        println!("  {what}");
        for line in platform.applied_hint(&startup.profile) {
            println!("  {line}");
        }
        platform.check_environment();
        Ok(())
    }
}
