defmodule RemoteAgentServerWeb.UserSocket do
  @moduledoc false

  use Phoenix.Socket

  channel("runner:*", RemoteAgentServerWeb.RunnerChannel)

  @roles ["runner", "mobile"]

  @impl true
  def connect(params, socket, _connect_info) do
    with {:ok, token} <- required_binary(params, "token"),
         {:ok, runner_id} <- required_binary(params, "runner_id"),
         {:ok, role} <- required_role(params),
         true <- valid_token?(token) do
      {:ok,
       socket
       |> assign(:runner_id, runner_id)
       |> assign(:role, role)}
    else
      _ -> :error
    end
  end

  @impl true
  def id(_socket), do: nil

  defp required_role(params) do
    case required_binary(params, "role") do
      {:ok, role} when role in @roles -> {:ok, role}
      _ -> :error
    end
  end

  defp required_binary(params, key) when is_map(params) do
    case Map.get(params, key) do
      value when is_binary(value) and byte_size(value) in 1..512 -> {:ok, value}
      _ -> :error
    end
  end

  defp required_binary(_params, _key), do: :error

  defp valid_token?(token) when is_binary(token) do
    case Application.get_env(:remote_agent_server, :relay_token) do
      configured when is_binary(configured) and byte_size(configured) > 0 ->
        Plug.Crypto.secure_compare(token, configured)

      _ ->
        false
    end
  end
end
