mod admin;
mod assets;
mod health_check;
mod login;

pub use admin::{
    alias_detail, create_alias, create_mailbox, dashboard, delete_alias, delete_mailbox, log_out,
    mailboxes, styleguide, toggle_alias,
};
pub use assets::{app_css, app_js};
pub use health_check::health_check;
pub use login::{home, login, login_form};
