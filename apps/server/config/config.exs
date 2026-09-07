import Config

config :remote_agent_server, RemoteAgentServerWeb.Endpoint,
  adapter: Bandit.PhoenixAdapter,
  url: [host: "localhost"],
  http: [ip: {127, 0, 0, 1}, port: 4000],
  check_origin: true,
  pubsub_server: RemoteAgentServer.PubSub,
  server: false

config :phoenix,
  json_library: Jason,
  filter_parameters: ["password", "token", "line", "data", "ticket"]
