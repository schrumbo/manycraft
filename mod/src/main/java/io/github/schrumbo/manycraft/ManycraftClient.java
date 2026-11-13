package io.github.schrumbo.manycraft;

import net.fabricmc.api.ClientModInitializer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/**
 * Client entrypoint. Connects the instance to the Manycraft window when launched by it.
 */
public final class ManycraftClient implements ClientModInitializer {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");

	@Override
	public void onInitializeClient() {
		Config config;
		try {
			config = Config.read(System::getProperty, System::getenv);
		} catch (RuntimeException e) {
			LOGGER.error("invalid manycraft launch properties, companion disabled", e);
			return;
		}
		if (config == null) {
			return;
		}
		Companion.start(config);
	}
}
