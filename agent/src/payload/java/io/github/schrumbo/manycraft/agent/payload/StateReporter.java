package io.github.schrumbo.manycraft.agent.payload;

import java.lang.reflect.Field;
import java.util.Objects;
import java.util.function.Consumer;
import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.screens.ConnectScreen;
import net.minecraft.client.gui.screens.DeathScreen;
import net.minecraft.client.gui.screens.DisconnectedScreen;
import net.minecraft.client.gui.screens.LevelLoadingScreen;
import net.minecraft.client.gui.screens.Screen;
import net.minecraft.client.multiplayer.ServerData;
import net.minecraft.client.multiplayer.resolver.ServerAddress;
import net.minecraft.network.DisconnectionDetails;
import net.minecraft.network.chat.Component;

final class StateReporter {
	private static final Field DETAILS = Access.field(DisconnectedScreen.class, "details");
	private static final Field CAUSE_OF_DEATH = Access.field(DeathScreen.class, "causeOfDeath");
	private final Consumer<String> sink;
	private final QueueDetector queue;
	private String kind = "menu";
	private String server;
	private String reason;
	private String cause;
	private ServerData pending;
	private ServerData lastServer;

	StateReporter(Consumer<String> sink, QueueDetector queue) {
		this.sink = sink;
		this.queue = queue;
	}

	synchronized String line() {
		return Protocol.state(kind, server, reason, cause);
	}

	synchronized ServerData lastServer() {
		return lastServer;
	}

	synchronized void noteConnecting(ServerData data) {
		pending = data;
		lastServer = data;
	}

	void tick(Minecraft mc) {
		Screen screen = Compat.screen(mc);
		String newKind;
		String newReason = null;
		String newCause = null;
		if (screen instanceof DisconnectedScreen disconnected) {
			newKind = "disconnected";
			DisconnectionDetails found = Access.get(DETAILS, disconnected);
			Component details = found != null ? found.reason() : disconnected.getTitle();
			newReason = details.getString();
			newCause = DisconnectClassifier.classify(details);
		} else if (screen instanceof DeathScreen death) {
			newKind = "dead";
			Component cause = Access.get(CAUSE_OF_DEATH, death);
			newReason = cause != null ? cause.getString() : death.getTitle().getString();
		} else if (screen instanceof ConnectScreen || screen instanceof LevelLoadingScreen || (mc.getConnection() != null && mc.player == null)) {
			newKind = "connecting";
		} else if (mc.level != null && mc.player != null) {
			newKind = queue.active(System.currentTimeMillis()) ? "queue" : "playing";
		} else {
			newKind = "menu";
		}
		String newServer = serverFor(mc, newKind);
		update(newKind, newServer, newReason, newCause);
	}

	private synchronized String serverFor(Minecraft mc, String newKind) {
		ServerData current = mc.getCurrentServer();
		if (current != null) {
			lastServer = current;
		}
		switch (newKind) {
			case "menu" -> {
				pending = null;
				return null;
			}
			case "playing", "queue", "dead" -> {
				if (mc.hasSingleplayerServer()) {
					return null;
				}
				return normalize(current != null ? current : pending);
			}
			default -> {
				return normalize(current != null ? current : pending != null ? pending : lastServer);
			}
		}
	}

	private void update(String newKind, String newServer, String newReason, String newCause) {
		String line;
		synchronized (this) {
			if (newKind.equals(kind) && Objects.equals(newServer, server) && Objects.equals(newReason, reason) && Objects.equals(newCause, cause)) {
				return;
			}
			kind = newKind;
			server = newServer;
			reason = newReason;
			cause = newCause;
			line = Protocol.state(kind, server, reason, cause);
		}
		sink.accept(line);
	}

	static String normalize(ServerData data) {
		if (data == null || data.ip == null) {
			return null;
		}
		ServerAddress address = ServerAddress.parseString(data.ip);
		return address.getHost() + ":" + address.getPort();
	}
}
