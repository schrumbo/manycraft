package io.github.schrumbo.manycraft.mixin;

import io.github.schrumbo.manycraft.CompanionHooks;
import net.minecraft.client.gui.screens.ConnectScreen;
import net.minecraft.client.multiplayer.ServerData;
import net.minecraft.client.multiplayer.resolver.ServerAddress;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(ConnectScreen.class)
abstract class ConnectScreenMixin {
	@Inject(method = "startConnecting", at = @At("HEAD"))
	private static void manycraft$noteServer(
		net.minecraft.client.gui.screens.Screen parent,
		net.minecraft.client.Minecraft minecraft,
		ServerAddress address,
		ServerData data,
		boolean quickPlay,
		net.minecraft.client.multiplayer.TransferState transferState,
		CallbackInfo ci
	) {
		CompanionHooks.noteConnecting(data);
	}
}
