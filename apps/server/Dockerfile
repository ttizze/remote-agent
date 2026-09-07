FROM hexpm/elixir:1.19.5-erlang-28.5.0.5-debian-bookworm-20260824-slim@sha256:6eed5992d974265841fd43857e21236d3c5a700de70caaff234b74f6682921fd AS build

# Cross-architecture local builds may need --build-arg 'ERL_FLAGS=+JMsingle true'.
ARG ERL_FLAGS
ENV MIX_ENV=prod
WORKDIR /app
RUN mix local.hex --force && mix local.rebar --force
COPY mix.exs mix.lock ./
COPY config/config.exs config/config.exs
RUN mix deps.get --only prod && mix deps.compile
COPY lib lib
COPY config/runtime.exs config/runtime.exs
RUN mix compile --warnings-as-errors && mix release

FROM debian:bookworm-20260824-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
RUN apt-get update \
    && apt-get install -y --no-install-recommends libstdc++6 openssl libncurses6 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
ENV LANG=C.UTF-8 \
    PHX_SERVER=true \
    PHX_BIND_IP=0.0.0.0 \
    PORT=4000 \
    RELEASE_DISTRIBUTION=none
WORKDIR /app
COPY --from=build --chown=nobody:nogroup /app/_build/prod/rel/remote_agent_server ./
USER nobody:nogroup
EXPOSE 4000
CMD ["/app/bin/remote_agent_server", "start"]
