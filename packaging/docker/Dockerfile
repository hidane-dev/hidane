# hidane as a container image: the static Linux binary of a release on an empty base, for
# linux/amd64 and linux/arm64. The release workflow builds it from this context:
#
#   amd64/hidane, arm64/hidane    the binaries of the x86_64 and aarch64 musl archives
#   LICENSE-MIT, LICENSE-APACHE, THIRD-PARTY-LICENSES.txt
#
#   docker run --rm -p 8080:8080 ghcr.io/hidane-dev/hidane
FROM scratch
ARG TARGETARCH
COPY ${TARGETARCH}/hidane /hidane
COPY LICENSE-MIT LICENSE-APACHE THIRD-PARTY-LICENSES.txt /licenses/
EXPOSE 8080
ENTRYPOINT ["/hidane"]
CMD ["--host", "0.0.0.0", "--port", "8080"]
