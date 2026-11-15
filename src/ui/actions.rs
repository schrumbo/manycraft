//! Commands the views dispatch and `ui::state` handles, so views never touch processes or files.

use gpui_kit::{Action, actions};
use schemars::JsonSchema;
use serde::Deserialize;

actions!(manycraft, [ShowGrid, ToggleFullscreen, NextInstance, PreviousInstance, NextAttention, StartAll, StopAll, ToggleSidebar, NewBatch, AddAccount, AddMicrosoft, Reload, OpenSettings, OpenChat, OpenPalette, ShowAll, ShowAccounts, SelectAll, ClearSelection, StartSelected, StopSelected, RestartSelected, ReconnectSelected, MessageSelected, ImportPrismAccounts, ImportFromLauncher, PickShortcutInstance, UndoRemoveAccount, Undo, CheckForUpdates, CancelUpdate, RestartToUpdate, PreviousVersion, DismissNews, ImportBatch, RenameCurrent, FindModpacks]);

/// Gives these accounts (ids) colour `color`, an index into `core::account::COLORS`.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct SetColor {
    pub accounts: Vec<String>,
    pub color: u8,
}

/// Shows the colours to pick from for this account (id) in the palette.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct PickColor(pub String);

/// Opens the sign-in sheet for these Microsoft accounts one after another.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct SignInEach(pub Vec<String>);

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

/// Cancels the bulk start of a batch, of every batch when `None`: what has not launched yet stops.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct CancelStart(pub Option<String>);

/// Starts the games of a batch, of every batch when `None`, that wait for free memory, whether they fit or not.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct StartAnyway(pub Option<String>);

/// Opens the batch sheet for an existing batch.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct EditBatch(pub String);

/// Signs a Microsoft account in again after its login expired.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct SignInAgain(pub String);

/// Opens the skin sheet of this Microsoft account (id).
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct ChangeSkin(pub String);

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

/// Writes a batch (id) as a `.mrpack` file the person picks.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct ExportBatch(pub String);

/// Opens a batch's shared folder (id) with its mods, config and packs in the file manager.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct OpenBatchFolder(pub String);

/// Opens one instance's game folder (key) in the file manager.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct OpenGameFolder(pub String);

/// Stops a running import by its number.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct CancelImport(pub u64);

/// Opens the sheet that creates an instance.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct NewInstance;

/// Shows the page of one instance (id).
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct ShowInstance(pub String);

/// Plays an instance alone with one account; without one, with the account that played it last.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct PlayInstance {
    pub instance: String,
    pub account: Option<String>,
}

/// Opens the batch sheet for a new batch on this instance (id).
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct NewBatchFor(pub String);

/// Opens the sheet that adds and removes the mods of an instance (id).
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct EditMods(pub String);

/// Copies an instance (id) with its mods, config and packs, but no worlds.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct CloneInstance(pub String);

/// Shows the page of an instance (id) and starts renaming it there.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct RenameInstance(pub String);

/// Asks, then deletes an instance (id) with its worlds.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct DeleteInstance(pub String);

/// Shows the settings of an instance with its advanced part open and one setting focused: `memory`, `java`,
/// `jvm_args`, `game_args`, `window`, `gpu` or `renderer`.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct OpenInstanceSetting {
    pub instance: String,
    pub setting: String,
}

/// Opens an instance's folder (id) in the file manager.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct OpenInstanceFolder(pub String);

/// Writes an instance (id) as a `.mrpack` file the person picks.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct ExportInstance(pub String);

/// Puts a desktop shortcut on the desktop that plays an instance, with an account (id) when one is named.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct CreateShortcut {
    pub instance: String,
    pub account: Option<String>,
}

/// Opens the mod browser of an instance (id) with the search focused.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct AddMods(pub String);

/// Opens the content browser of this instance (id) on resource packs.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct FindResourcePacks(pub String);

/// Opens the content browser of this instance (id) on shaders.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct FindShaders(pub String);

/// Asks, then sets an instance (id) back to its mods, config and packs from when it was created. Worlds stay.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct ResetInstance(pub String);

/// Looks for a newer stable loader for an instance (id), asks, then switches to it.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct UpdateLoader(pub String);
