use clap::Args;
use envc::{Ctx, Frame, Result, Source, Stack, StateFile, Startup};

#[derive(Args, Debug)]
pub struct Autoload;

impl Autoload {
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        let env = &ctx.env;
        let Some(profile) = Startup::remembered(env)? else {
            return Ok(());
        };

        let file = env.profile_env_file(&profile)?;
        if !file.is_file() {
            eprintln!(
                "envc: startup profile '{profile}' is selected but {} is missing; skipping autoload",
                env.tilde(&file)
            );
            return Ok(());
        }

        let source = Source::load(env, &file, &env.vars)?;
        source.archive(env, &profile)?;
        let stack = Stack {
            frames: vec![Frame::capture(&profile, &env.vars, &source.assignments, Some(source.digest))],
        };
        let mut lines: Vec<String> = stack.frames[0].apply_lines(ctx.shell).collect();
        lines.push(stack.active_line(ctx.shell));
        ctx.deliver("autoload", &lines)?;
        stack.save(env)?;
        stack.frames[0].record(env, "autoload")
    }
}
