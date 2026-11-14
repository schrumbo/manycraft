package io.github.schrumbo.manycraft.mixin;

import io.github.schrumbo.manycraft.CompanionHooks;
import net.minecraft.client.gui.screens.multiplayer.JoinMultiplayerScreen;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(JoinMultiplayerScreen.class)
abstract class JoinMultiplayerScreenMixin {
	@Inject(method = "init", at = @At("TAIL"))
	private void manycraft$track(CallbackInfo ci) {
		CompanionHooks.multiplayerOpened((JoinMultiplayerScreen) (Object) this);
	}
}
