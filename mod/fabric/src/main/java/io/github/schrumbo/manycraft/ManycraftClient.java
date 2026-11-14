package io.github.schrumbo.manycraft;

import net.fabricmc.api.ClientModInitializer;
import net.fabricmc.fabric.api.client.event.lifecycle.v1.ClientLifecycleEvents;
import net.fabricmc.fabric.api.client.event.lifecycle.v1.ClientTickEvents;
import net.fabricmc.fabric.api.client.message.v1.ClientReceiveMessageEvents;
import net.fabricmc.fabric.api.client.rendering.v1.hud.HudElementRegistry;
import net.fabricmc.loader.api.FabricLoader;

/**
 * Client entrypoint. Connects the instance to the Manycraft window when launched by it.
 */
public final class ManycraftClient implements ClientModInitializer {
	@Override
	public void onInitializeClient() {
		String version = FabricLoader.getInstance().getModContainer("manycraft")
			.map(c -> c.getMetadata().getVersion().getFriendlyString())
			.orElse("unknown");
		Companion.start(version, companion -> {
			ClientLifecycleEvents.CLIENT_STARTED.register(companion::started);
			ClientTickEvents.END_CLIENT_TICK.register(companion::tick);
			ClientReceiveMessageEvents.GAME.register(companion.chat()::onGame);
			ClientReceiveMessageEvents.CHAT.register(companion.chat()::onChat);
			HudElementRegistry.addLast(GameIdentity.BADGE, companion.identity()::extractBadge);
		});
	}
}
