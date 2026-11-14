package io.github.schrumbo.manycraft.mixin;

import io.github.schrumbo.manycraft.CompanionHooks;
import net.minecraft.client.multiplayer.ServerList;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(ServerList.class)
abstract class ServerListMixin {
	@Inject(method = "save", at = @At("TAIL"))
	private void manycraft$shareServers(CallbackInfo ci) {
		CompanionHooks.serversSaved();
	}
}
