package io.github.schrumbo.manycraft;

import java.time.Instant;
import net.minecraft.client.Minecraft;
import net.minecraft.network.chat.Component;
import net.minecraftforge.client.event.AddGuiOverlayLayersEvent;
import net.minecraftforge.client.event.ClientChatReceivedEvent;
import net.minecraftforge.client.event.SystemMessageReceivedEvent;
import net.minecraftforge.event.TickEvent;
import net.minecraftforge.eventbus.api.listener.Priority;
import net.minecraftforge.fml.common.Mod;
import net.minecraftforge.fml.javafmlmod.FMLJavaModLoadingContext;

/**
 * Client entrypoint. Connects the instance to the Manycraft window when launched by it.
 */
@Mod("manycraft")
public final class ManycraftForge {
	private boolean started;

	/**
	 * Action bar text, which Forge raises no event for.
	 */
	public static void overlay(Component message) {
		Companion c = Companion.get();
		if (c != null) {
			c.chat().onGame(message, true);
		}
	}

	public ManycraftForge(FMLJavaModLoadingContext context) {
		Companion.start(context.getContainer().getModInfo().getVersion().toString(), companion -> {
			TickEvent.ClientTickEvent.Pre.BUS.addListener(e -> {
				if (!started) {
					started = true;
					companion.started(Minecraft.getInstance());
				}
			});
			TickEvent.ClientTickEvent.Post.BUS.addListener(e -> companion.tick(Minecraft.getInstance()));
			SystemMessageReceivedEvent.BUS.addListener(Priority.LOWEST, e -> {
				companion.chat().onGame(e.getMessage(), false);
			});
			ClientChatReceivedEvent.Player.BUS.addListener(Priority.LOWEST, e -> {
				companion.chat().onChat(e.getMessage(), e.getPlayerChatMessage(), null, e.getBoundChatType(), Instant.now());
			});
			ClientChatReceivedEvent.BUS.addListener(Priority.LOWEST, e -> {
				if (e.getClass() == ClientChatReceivedEvent.class && e.getBoundChatType() != null) {
					companion.chat().onChat(e.getMessage(), null, null, e.getBoundChatType(), Instant.now());
				}
			});
			AddGuiOverlayLayersEvent.BUS.addListener(e -> e.getLayeredDraw().add(GameIdentity.BADGE, companion.identity()::extractBadge));
		});
	}
}
