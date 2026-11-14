package io.github.schrumbo.manycraft;

import java.io.BufferedReader;
import java.io.BufferedWriter;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStreamWriter;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.LinkedBlockingDeque;
import java.util.concurrent.TimeUnit;
import java.util.function.Consumer;
import java.util.function.Supplier;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class ManagerLink {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final long BACKOFF_MIN_MS = 250L;
	private static final long BACKOFF_MAX_MS = 5000L;

	private final Config config;
	private final Supplier<String[]> handshake;
	private final Consumer<Protocol.Incoming> inbound;
	private final LinkedBlockingDeque<String> outbound = new LinkedBlockingDeque<>(2048);
	private volatile boolean connected;
	private volatile boolean stopped;
	private volatile Thread thread;

	ManagerLink(Config config, Supplier<String[]> handshake, Consumer<Protocol.Incoming> inbound) {
		this.config = config;
		this.handshake = handshake;
		this.inbound = inbound;
	}

	void start() {
		Thread created = new Thread(this::run, "manycraft-link");
		created.setDaemon(true);
		thread = created;
		created.start();
	}

	void ensureRunning() {
		Thread current = thread;
		if (!stopped && (current == null || !current.isAlive())) {
			LOGGER.warn("manager link thread was gone, restarting it");
			start();
		}
	}

	void stop() {
		stopped = true;
	}

	boolean send(String line) {
		return connected && outbound.offer(line);
	}

	private void run() {
		long backoff = BACKOFF_MIN_MS;
		while (!stopped) {
			boolean everConnected = false;
			try (Socket socket = new Socket()) {
				socket.connect(new InetSocketAddress(config.host(), config.port()), 2000);
				socket.setTcpNoDelay(true);
				everConnected = true;
				session(socket);
			} catch (IOException | RuntimeException | OutOfMemoryError e) {
				LOGGER.debug("manager link: {}", e.toString());
			} finally {
				connected = false;
			}
			if (everConnected) {
				backoff = BACKOFF_MIN_MS;
			}
			try {
				Thread.sleep(backoff);
			} catch (InterruptedException e) {
				return;
			}
			backoff = Math.min(backoff * 2, BACKOFF_MAX_MS);
		}
	}

	private void session(Socket socket) throws IOException {
		outbound.clear();
		connected = true;
		String[] opening = handshake.get();
		for (int i = opening.length - 1; i >= 0; i--) {
			outbound.addFirst(opening[i]);
		}
		BufferedWriter writer = new BufferedWriter(new OutputStreamWriter(socket.getOutputStream(), StandardCharsets.UTF_8));
		Thread writerThread = new Thread(() -> pump(writer, socket), "manycraft-link-writer");
		writerThread.setDaemon(true);
		writerThread.start();
		try (BufferedReader reader = new BufferedReader(new InputStreamReader(socket.getInputStream(), StandardCharsets.UTF_8))) {
			String line;
			while (!stopped && (line = reader.readLine()) != null) {
				Protocol.parse(line).ifPresent(inbound);
			}
		} finally {
			connected = false;
			writerThread.interrupt();
		}
	}

	private void pump(BufferedWriter writer, Socket socket) {
		try {
			while (!stopped && connected) {
				String line = outbound.poll(500, TimeUnit.MILLISECONDS);
				if (line == null) {
					continue;
				}
				writer.write(line);
				writer.write('\n');
				while ((line = outbound.poll()) != null) {
					writer.write(line);
					writer.write('\n');
				}
				writer.flush();
			}
		} catch (IOException | InterruptedException e) {
			try {
				socket.close();
			} catch (IOException ignored) {
			}
		}
	}
}
