package io.github.schrumbo.manycraft.agent.payload;

import java.lang.reflect.Field;
import java.net.MalformedURLException;
import java.net.URI;
import java.net.URISyntaxException;
import java.net.URL;
import java.util.ArrayList;
import java.util.List;
import java.util.UUID;
import net.minecraft.client.Minecraft;
import net.minecraft.client.multiplayer.ClientCommonPacketListenerImpl;
import net.minecraft.client.multiplayer.ServerData;
import net.minecraft.client.resources.server.DownloadedPackSource;
import net.minecraft.network.Connection;
import net.minecraft.network.chat.Component;
import net.minecraft.network.protocol.common.ClientboundResourcePackPushPacket;

final class PackOffers {
	static final String DECLINED = "multiplayer.requiredTexturePrompt.disconnect";
	private static final Field CONNECTION = Access.field(ClientCommonPacketListenerImpl.class, "connection");
	private static final Field SERVER_DATA = Access.field(ClientCommonPacketListenerImpl.class, "serverData");

	record Request(UUID id, URL url, String hash) {}

	record Held(Protocol.Offer offer, ClientCommonPacketListenerImpl listener, List<Request> requests) {}

	private volatile String policy = "ask";
	private volatile Held held;

	void policy(String value) {
		if (value.equals("ask") || value.equals("accept") || value.equals("decline")) {
			policy = value;
		}
	}

	Protocol.Offer offer() {
		Held h = held;
		return h == null ? null : h.offer();
	}

	boolean push(Minecraft mc, ClientCommonPacketListenerImpl listener, ClientboundResourcePackPushPacket packet, boolean shown) {
		if (!mc.isSameThread()) {
			return false;
		}
		URL url = url(packet.url());
		if (url == null) {
			return false;
		}
		Request request = new Request(packet.id(), url, packet.hash());
		String text = packet.prompt().map(Component::getString).orElse(null);
		switch (policy) {
			case "accept" -> apply(mc, listener, List.of(request), packet.required(), true);
			case "decline" -> apply(mc, listener, List.of(request), packet.required(), false);
			default -> {
				if (shown || !prompts(listener, packet.required())) {
					return false;
				}
				Held h = held;
				if (h != null && h.listener() == listener) {
					List<Request> requests = new ArrayList<>(h.requests());
					requests.add(request);
					held = new Held(new Protocol.Offer(h.offer().id(), h.offer().required() || packet.required(), text != null ? text : h.offer().text()), listener, List.copyOf(requests));
				} else {
					held = new Held(new Protocol.Offer(packet.id().toString(), packet.required(), text), listener, List.of(request));
				}
			}
		}
		return true;
	}

	void answer(Minecraft mc, String id, boolean accept) {
		Held h = held;
		if (h == null || !h.offer().id().equals(id)) {
			return;
		}
		held = null;
		if (connected(h.listener())) {
			apply(mc, h.listener(), h.requests(), h.offer().required(), accept);
		}
	}

	void tick() {
		Held h = held;
		if (h != null && !connected(h.listener())) {
			held = null;
		}
	}

	private static boolean prompts(ClientCommonPacketListenerImpl listener, boolean required) {
		ServerData data = Access.get(SERVER_DATA, listener);
		ServerData.ServerPackStatus status = data != null ? data.getResourcePackStatus() : ServerData.ServerPackStatus.PROMPT;
		return status == ServerData.ServerPackStatus.PROMPT || required && status == ServerData.ServerPackStatus.DISABLED;
	}

	private static void apply(Minecraft mc, ClientCommonPacketListenerImpl listener, List<Request> requests, boolean required, boolean accept) {
		DownloadedPackSource source = mc.getDownloadedPackSource();
		if (accept) {
			source.allowServerPacks();
		} else {
			source.rejectServerPacks();
			if (required) {
				Connection connection = Access.get(CONNECTION, listener);
				if (connection != null) {
					connection.disconnect(Component.translatable(DECLINED));
				}
			}
		}
		for (Request r : requests) {
			source.pushPack(r.id(), r.url(), r.hash());
		}
	}

	private static boolean connected(ClientCommonPacketListenerImpl listener) {
		Connection connection = Access.get(CONNECTION, listener);
		return connection != null && connection.isConnected();
	}

	private static URL url(String text) {
		try {
			URL url = new URI(text).toURL();
			return url.getProtocol().equals("http") || url.getProtocol().equals("https") ? url : null;
		} catch (URISyntaxException | MalformedURLException | IllegalArgumentException e) {
			return null;
		}
	}
}
