//! Commands the views dispatch and `ui::state` handles, so views never touch processes or files.

use gpui_kit::{Action, actions};
use schemars::JsonSchema;
use serde::Deserialize;

actions!(manycraft, [ShowGrid, NextInstance, PreviousInstance, NextAttention, StartAll, StopAll, ToggleSidebar, TileLarger, TileSmaller, NewBatch, AddAccount, AddMicrosoft, Reload, OpenSettings, OpenChat, OpenPalette, ShowAll, ShowAccounts, SelectAll, ClearSelection, StartSelected, StopSelected, RestartSelected, ReconnectSelected, MessageSelected]);

/// Brings this instance's game window into the manager's place.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct Play(pub String);

/// Starts one instance by key.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct Start(pub String);

/// Stops one instance by key.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct Stop(pub String);

/// Joins the last server again.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct Reconnect(pub String);

/// Respawns a dead player.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct Respawn(pub String);

/// Starts every account of a batch, staggered.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct StartBatch(pub String);

/// Stops every instance of a batch.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct StopBatch(pub String);

/// Opens the batch sheet for an existing batch.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct EditBatch(pub String);

/// Signs a Microsoft account in again after its login expired.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct SignInAgain(pub String);

/// Sends a chat line or command to every running instance of a batch.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct Broadcast {
    pub batch: String,
    pub text: String,
}

/// Sends a chat line or command to one running instance.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct SendChat {
    pub key: String,
    pub text: String,
}

/// Shows the grid of one batch.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct ShowBatch(pub String);

/// Starts these instances, staggered within each batch.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct StartMany(pub Vec<String>);

/// Stops these instances.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct StopMany(pub Vec<String>);

/// Stops the running ones of these instances and starts them again once they exited; starts the others.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct RestartMany(pub Vec<String>);

/// Joins the last server again on each of these instances that runs.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct ReconnectMany(pub Vec<String>);

/// Sends one chat line or command to each of these instances that is on a server.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct SendChatMany {
    pub keys: Vec<String>,
    pub text: String,
}
