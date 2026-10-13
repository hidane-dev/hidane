package dev.hidane.testcontainers;

import com.google.cloud.firestore.Firestore;
import org.junit.jupiter.api.Test;
import org.testcontainers.containers.GenericContainer;
import org.testcontainers.containers.wait.strategy.Wait;
import org.testcontainers.utility.DockerImageName;

/**
 * hidane's image as a GenericContainer, with the image's own command
 * ({@code --host 0.0.0.0 --port 8080}), waiting for the last line of its banner. The workflow runs
 * this against the published v0.4.0 image too.
 */
class GenericContainerTest {
    @Test
    void aGenericContainerStartsHidane() throws Exception {
        try (GenericContainer<?> hidane = new GenericContainer<>(DockerImageName.parse(Operations.image()))
                .withExposedPorts(8080)
                .waitingFor(Wait.forLogMessage(".*Dev App Server is now running.*", 1))) {
            hidane.start();
            try (Firestore db = Operations.client(hidane.getHost() + ":" + hidane.getMappedPort(8080))) {
                Operations.run(db);
            }
        }
    }
}
