//! Who may do what: every route is held to one rule, nobody signed in, anybody signed in, or an Administrator
//! (`routes`, "Access" in DECISIONS.md). An Administrator (`users.role = 'admin'`) may do everything.

#[cfg(test)]
pub mod api_tests;
pub mod routes;
