package io.github.schrumbo.manycraft.mixin;

import com.llamalad7.mixinextras.injector.wrapoperation.Operation;
import com.llamalad7.mixinextras.injector.wrapoperation.WrapOperation;
import io.github.schrumbo.manycraft.CompanionHooks;
import net.minecraft.client.Minecraft;
import net.minecraft.client.renderer.GameRenderer;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(Minecraft.class)
abstract class MinecraftMixin {
	@WrapOperation(method = "renderFrame", at = @At(value = "INVOKE", target = "Lnet/minecraft/client/renderer/GameRenderer;render()V"))
	private void manycraft$render(GameRenderer renderer, Operation<Void> original) {
		CompanionHooks.render(renderer, () -> original.call(renderer));
	}

	@Inject(method = "pauseIfInactive", at = @At("HEAD"), cancellable = true)
	private void manycraft$noPauseWhileHidden(CallbackInfo ci) {
		if (CompanionHooks.hidden()) {
			ci.cancel();
		}
	}
}
