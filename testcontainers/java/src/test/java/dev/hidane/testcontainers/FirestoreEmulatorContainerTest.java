package dev.hidane.testcontainers;

import static org.junit.jupiter.api.Assertions.assertTrue;

import com.google.cloud.firestore.Firestore;
import org.junit.jupiter.api.Test;
import org.testcontainers.gcloud.FirestoreEmulatorContainer;
import org.testcontainers.utility.DockerImageName;

/**
 * hidane's image in place of the Cloud SDK image, in the gcloud module's
 * FirestoreEmulatorContainer, which starts it with
 * {@code /bin/sh -c "gcloud beta emulators firestore start --host-port 0.0.0.0:8080 <flags>"}
 * and waits for a log line with "running".
 */
class FirestoreEmulatorContainerTest {
    // The module accepts the Cloud SDK images only, unless the image is declared compatible.
    static final DockerImageName HIDANE = DockerImageName.parse(Operations.image())
            .asCompatibleSubstituteFor("gcr.io/google.com/cloudsdktool/google-cloud-cli");

    @Test
    void theGcloudModuleStartsHidane() throws Exception {
        try (FirestoreEmulatorContainer emulator = new FirestoreEmulatorContainer(HIDANE)) {
            emulator.start();
            try (Firestore db = Operations.client(emulator.getEmulatorEndpoint())) {
                Operations.run(db);
            }
        }
    }

    @Test
    void withFlags() throws Exception {
        try (FirestoreEmulatorContainer emulator =
                new FirestoreEmulatorContainer(HIDANE).withFlags("--project=demo-testcontainers --quiet")) {
            emulator.start();
            assertTrue(
                    emulator.getLogs().contains("in place of `gcloud emulators firestore start`, with: "
                            + "hidane --host 0.0.0.0 --port 8080"),
                    emulator.getLogs());
            try (Firestore db = Operations.client(emulator.getEmulatorEndpoint())) {
                Operations.run(db);
            }
        }
    }
}
