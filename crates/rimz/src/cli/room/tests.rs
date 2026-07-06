use super::{ResumePromptMode, resume_prompt_mode};

#[test]
fn resume_prompt_mode_uses_tty_or_confirm_flag() {
    assert_eq!(
        resume_prompt_mode(false, true),
        ResumePromptMode::Interactive
    );
    assert_eq!(
        resume_prompt_mode(true, false),
        ResumePromptMode::Interactive
    );
    assert_eq!(resume_prompt_mode(false, false), ResumePromptMode::Silent);
}
