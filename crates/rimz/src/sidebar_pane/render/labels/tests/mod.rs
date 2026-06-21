use super::super::fmt;
use super::*;

mod glyphs;
mod meters;

#[test]
fn heat_fraction_saturates_at_attention_age_ceiling() {
    let ceiling = crate::agents::ATTENTION_AGE_CEILING_SECS;
    assert_eq!(heat_fraction(ceiling), Some(1.0));
    assert_eq!(heat_fraction(ceiling * 2), Some(1.0));
}
