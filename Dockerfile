# Stage 1: Build env
FROM rust:1.80-slim AS builder

WORKDIR /app
COPY . .

RUN cargo build --release --bin fhirstream_server

# Stage 2: Minimalist runtime
FROM gcr.io/distroless/cc-debian12

COPY --from=builder /app/target/release/fhirstream_server /usr/local/bin/fhirstream_server

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/fhirstream_server"]
