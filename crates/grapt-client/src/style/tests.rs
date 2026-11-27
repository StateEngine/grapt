use super::*;

#[test]
fn styles_compose_without_naming_a_colour_twice() {
    let style = Style::token(Token::DiffAdd)
        .on(Paint::Indexed(22))
        .with(Emphasis::BOLD)
        .with(Emphasis::UNDERLINE);
    assert_eq!(style.fg, Paint::Token(Token::DiffAdd));
    assert_eq!(style.bg, Paint::Indexed(22));
    assert!(style
        .emphasis
        .contains(Emphasis::BOLD | Emphasis::UNDERLINE));
    assert!(!style.emphasis.contains(Emphasis::ITALIC));
    assert!(Emphasis::NONE.is_empty());
    assert!(Style::default().is_plain());
    assert!(!Style::PLAIN.with(Emphasis::DIM).is_plain());
}
