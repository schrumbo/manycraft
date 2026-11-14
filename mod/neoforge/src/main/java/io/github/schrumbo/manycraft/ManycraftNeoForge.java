package io.github.schrumbo.manycraft;

import java.time.Instant;
import net.minecraft.client.Minecraft;
import net.neoforged.api.distmarker.Dist;
import net.neoforged.bus.api.EventPriority;
import net.neoforged.bus.api.IEventBus;
import net.neoforged.fml.ModContainer;
import net.neoforged.fml.common.Mod;
import net.neoforged.neoforge.client.event.ClientChatReceivedEvent;
import net.neoforged.neoforge.client.event.ClientTickEvent;
import net.neoforged.neoforge.client.event.RegisterGuiLayersEvent;
import net.neoforged.neoforge.client.event.lifecycle.ClientStartedEvent;
import net.neoforged.neoforge.common.NeoForge;

/**
 * Client entrypoint. Connects the instance to the Manycraft window when launched by it.
 */
@Mod(value = "manycraft", dist = Dist.CLIENT)
public final class ManycraftNeoForge {
	public ManycraftNeoForge(IEventBus modBus, ModContainer container) {
		Companion.start(container.getModInfo().getVersion().toString(), companion -> {
			IEventBus bus = NeoForge.EVENT_BUS;
			bus.addListener(ClientStartedEvent.class, e -> companion.started(e.getClient()));
			bus.addListener(ClientTickEvent.Post.class, e -> companion.tick(Minecraft.getInstance()));
			bus.addListener(EventPriority.LOWEST, ClientChatReceivedEvent.System.class, e -> companion.chat().onGame(e.getMessage(), e.isOverlay()));
			bus.addListener(EventPriority.LOWEST, ClientChatReceivedEvent.Player.class, e -> companion.chat().onChat(e.getMessage(), e.getPlayerChatMessage(), null, e.getBoundChatType(), Instant.now()));
			bus.addListener(EventPriority.LOWEST, ClientChatReceivedEvent.class, e -> {
				if (e.getClass() == ClientChatReceivedEvent.class && e.getBoundChatType() != null) {
					companion.chat().onChat(e.getMessage(), null, null, e.getBoundChatType(), Instant.now());
				}
			});
			modBus.addListener(RegisterGuiLayersEvent.class, e -> e.registerAboveAll(GameIdentity.BADGE, companion.identity()::extractBadge));
		});
	}
}
