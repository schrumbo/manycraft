package io.github.schrumbo.manycraft.mixin;

import com.llamalad7.mixinextras.injector.wrapmethod.WrapMethod;
import com.llamalad7.mixinextras.injector.wrapoperation.Operation;
import io.github.schrumbo.manycraft.CompanionHooks;
import net.minecraft.client.Options;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(Options.class)
abstract class OptionsMixin {
	@Inject(method = "save", at = @At("HEAD"), cancellable = true)
	private void manycraft$guardSave(CallbackInfo ci) {
		if (CompanionHooks.blocksSave()) {
			ci.cancel();
		}
	}

	@WrapMethod(method = "save")
	private void manycraft$save(Operation<Void> original) {
		CompanionHooks.saveOptions((Options) (Object) this, () -> original.call());
	}
}
