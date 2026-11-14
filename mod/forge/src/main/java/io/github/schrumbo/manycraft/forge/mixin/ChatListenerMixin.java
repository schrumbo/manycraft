package io.github.schrumbo.manycraft.forge.mixin;

import io.github.schrumbo.manycraft.ManycraftForge;
import net.minecraft.client.multiplayer.chat.ChatListener;
import net.minecraft.network.chat.Component;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(ChatListener.class)
abstract class ChatListenerMixin {
	@Inject(method = "handleOverlay", at = @At("HEAD"))
	private void manycraft$overlay(Component message, CallbackInfo ci) {
		ManycraftForge.overlay(message);
	}
}
