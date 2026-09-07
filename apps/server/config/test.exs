import Config

config :remote_agent_server, RemoteAgentServerWeb.Endpoint,
  http: [ip: {127, 0, 0, 1}, port: 0],
  server: false
