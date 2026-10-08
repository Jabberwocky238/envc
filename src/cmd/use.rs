use anyhow::anyhow;
use clap::Args;
use envc::{Ctx, Frame, ProfileName, Result, Source, Stack, StateFile};

#[derive(Args, Debug)]
pub struct Use {
    #[arg(help = "Profile to push on top of the stack")]
    name: String,

    #[arg(short, long, help = "Go ahead without asking when a profile on the stack has changed")]
    yes: bool,
}

impl Use {
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        let ProfileName(name) = self.name.as_str().try_into()?;
        let env = &ctx.env;
        let file = env.existing_profile(name)?;
        let mut stack = Stack::load(env)?.unwrap_or_default();

        if let Some(i) = stack.position(name) {
            return Err(anyhow!(
                "'{name}' is already in use (frame {} of {}); `envc unuse {name}` first",
                i + 1,
                stack.frames.len()
            ));
        }

        ctx.confirm_drift(&stack.frames, self.yes)?;

        let source = Source::load(env, &file, &env.vars)?;
        source.archive(env, name)?;
        let frame = Frame::capture(name, &env.vars, &source.assignments, Some(source.digest));
        let mut lines: Vec<String> = frame.apply_lines(ctx.shell).collect();
        stack.frames.push(frame);
        lines.push(stack.active_line(ctx.shell));

        ctx.deliver("use", &lines)?;
        stack.store(env)?;
        stack.frames[stack.frames.len() - 1].record(env, "push")?;

        ctx.notify(format!("pushed '{name}' ({} vars): {}", stack.frames[stack.frames.len() - 1].entries.len(), stack.chain()));
        ctx.hint(&format!("envc use {name}"));
        Ok(())
    }
}
