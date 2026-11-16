package io.github.schrumbo.manycraft.agent;

/**
 * What the payload implements for the patched game code; game types travel as {@code Object} because this side cannot
 * see them.
 */
public interface Bridge {
	void frame(Object minecraft);

	boolean skipFrame(Object minecraft);

	void tick(Object minecraft);

	boolean beforeRender(Object renderer);

	void afterRender(Object renderer);

	String title();

	boolean managed();

	int hiddenFramerate();

	void audioClosed();

	boolean saveOptions(Object options);

	void multiplayerOpened(Object screen);

	void serversSaved();

	void closeRequested();

	boolean key(int action, Object event);

	void connecting(Object serverData);

	void channel(Object channel);

	Object remoteName(Object address);

	void systemChat(Object message, boolean overlay);

	void playerChat(Object bound, Object signed, Object decorated, Object sender, Object receivedAt);

	void disguisedChat(Object message, Object bound);

	void overlayChat(Object message);

	void hud(Object graphics);
}
