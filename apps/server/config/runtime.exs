import Config

port =
  case System.get_env("PORT") do
    nil ->
      nil

    value ->
      case Integer.parse(value) do
        {port, ""} when port in 0..65_535 -> port
        _ -> raise "PORT must be an integer between 0 and 65535"
      end
  end

bind_ip =
  case System.get_env("PHX_BIND_IP") do
    nil ->
      nil

    value ->
      case :inet.parse_address(String.to_charlist(value)) do
        {:ok, address} -> address
        {:error, :einval} -> raise "PHX_BIND_IP must be a valid IPv4 or IPv6 address"
      end
  end

if port != nil or bind_ip != nil do
  config :remote_agent_server, RemoteAgentServerWeb.Endpoint,
    http: [ip: bind_ip || {127, 0, 0, 1}, port: port || 4000]
end

if System.get_env("PHX_SERVER") in ["true", "1", "yes"] do
  config :remote_agent_server, RemoteAgentServerWeb.Endpoint, server: true
end

if host = System.get_env("PHX_HOST") do
  config :remote_agent_server, RemoteAgentServerWeb.Endpoint, url: [host: host]
end

if secret_key_base = System.get_env("SECRET_KEY_BASE") do
  config :remote_agent_server, RemoteAgentServerWeb.Endpoint, secret_key_base: secret_key_base
end

# A missing value intentionally keeps every socket connection unauthorized.
if relay_token = System.get_env("REMOTE_AGENT_RELAY_TOKEN") do
  config :remote_agent_server, :relay_token, relay_token
end
