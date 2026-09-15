//! What the Accounts screen actually draws.
//!
//! The state tests can only say the fields changed. These render a real frame
//! and read the characters back, which is the difference between the screen
//! existing and the screen being legible.

use super::*;
use mindplayer_core::accounts::{Account, MULTI_ACCOUNT_AGENTS};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

/// Every character the frame drew, joined row by row.
fn painted(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| crate::ui::render(f, app))
        .expect("the frame did not draw");
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn app_on_main() -> App {
    let mut app = App::new_in(std::env::temp_dir());
    app.screen = Screen::Main;
    app.accounts = MULTI_ACCOUNT_AGENTS.map(Account::inherited).to_vec();
    app
}

#[test]
fn the_screen_shows_every_provider_and_its_logins() {
    let mut app = app_on_main();
    app.open_accounts();
    let screen = painted(&mut app, 100, 30);

    assert!(screen.contains("Accounts"), "no title drawn:\n{screen}");
    for agent in MULTI_ACCOUNT_AGENTS {
        assert!(
            screen.contains(&agent.as_str().to_uppercase()),
            "{} has no heading:\n{screen}",
            agent.as_str()
        );
    }
    assert!(
        screen.contains("default"),
        "the login this machine already had is not listed:\n{screen}"
    );
    assert!(
        screen.contains("a add"),
        "the screen does not say how to add one:\n{screen}"
    );
}

/// The screen has to answer "which account does my next session use" without
/// the reader translating a role name into that answer.
#[test]
fn the_account_a_new_session_takes_is_the_one_marked_in_use() {
    let mut app = app_on_main();
    let second = Account::isolated(&std::env::temp_dir(), Agent::Codex, "work").unwrap();
    app.accounts.push(second);
    app.accounts[1].disabled = true;
    app.open_accounts();
    let screen = painted(&mut app, 100, 30);

    assert!(
        screen.contains("off"),
        "a disabled account looks usable:\n{screen}"
    );
    assert!(
        screen.contains("reserve"),
        "the account not in use looks the same as the one that is:\n{screen}"
    );
    assert!(
        screen.contains("in use"),
        "nothing says which account a new session takes:\n{screen}"
    );
    assert_eq!(
        screen.matches("in use").count(),
        // One per provider that still has a usable account.
        MULTI_ACCOUNT_AGENTS.len() - 1,
        "more than one account of a provider reads as the one in use:\n{screen}"
    );
}

#[test]
fn typing_a_name_replaces_the_list_rather_than_crowding_it() {
    let mut app = app_on_main();
    app.open_accounts();
    app.accounts_start_add();
    for c in "work".chars() {
        app.accounts_name_push(c);
    }
    let screen = painted(&mut app, 100, 30);

    assert!(
        screen.contains("work"),
        "the typed name is not shown:\n{screen}"
    );
    assert!(
        screen.contains("Add account"),
        "no title for the name prompt:\n{screen}"
    );
    assert!(
        !screen.contains("CODEX"),
        "the list is still behind the prompt:\n{screen}"
    );
}

#[test]
fn a_refusal_is_shown_where_the_action_was_taken() {
    let mut app = app_on_main();
    app.open_accounts();
    app.accounts_panel.as_mut().unwrap().selected = 1;
    app.accounts_remove();
    let screen = painted(&mut app, 100, 30);

    assert!(
        screen.contains("cannot be removed"),
        "the refusal never reached the screen:\n{screen}"
    );
}

#[test]
fn the_screen_still_fits_a_narrow_terminal() {
    let mut app = app_on_main();
    app.open_accounts();
    // Narrower than the popup wants, which is where a fixed width would panic.
    let screen = painted(&mut app, 40, 14);
    assert!(screen.contains("Accounts"), "{screen}");
}

#[test]
fn a_second_account_sits_under_its_provider_and_the_cursor_marks_one() {
    let mut app = app_on_main();
    app.accounts
        .push(Account::isolated(&std::env::temp_dir(), Agent::Codex, "overflow").unwrap());
    app.open_accounts();
    app.accounts_panel.as_mut().unwrap().selected = 2;
    let screen = painted(&mut app, 92, 22);

    let codex_at = screen.find(" CODEX").expect("no codex heading");
    let claude_at = screen.find(" CLAUDE").expect("no claude heading");
    let second_at = screen
        .find("overflow")
        .expect("the second account is missing");
    assert!(
        codex_at < second_at && second_at < claude_at,
        "the second account is not under its own provider:\n{screen}"
    );
    assert!(
        screen.contains("▶ overflow"),
        "nothing marks where the cursor is:\n{screen}"
    );
}
