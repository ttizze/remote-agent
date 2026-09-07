defmodule RemoteAgentServer.RuntimeConfigTest do
  use ExUnit.Case, async: false

  @runtime_config Path.expand("../config/runtime.exs", __DIR__)
  @env_keys ["PORT", "PHX_SERVER", "PHX_BIND_IP", "PHX_HOST", "SECRET_KEY_BASE"]

  test "configures the endpoint from runtime environment" do
    with_environment(
      %{
        "PORT" => "43123",
        "PHX_SERVER" => "true",
        "PHX_BIND_IP" => "0.0.0.0",
        "PHX_HOST" => "relay.example.test"
      },
      fn ->
        endpoint_config = endpoint_config()

        assert endpoint_config[:http] == [ip: {0, 0, 0, 0}, port: 43_123]
        assert endpoint_config[:server] == true
        assert endpoint_config[:url] == [host: "relay.example.test"]
      end
    )
  end

  test "leaves endpoint defaults alone when runtime overrides are absent" do
    with_environment(%{}, fn ->
      assert endpoint_config() == []
    end)
  end

  defp endpoint_config do
    @runtime_config
    |> Config.Reader.read!()
    |> Keyword.get(:remote_agent_server, [])
    |> Keyword.get(RemoteAgentServerWeb.Endpoint, [])
  end

  defp with_environment(values, fun) do
    previous = Map.new(@env_keys, &{&1, System.get_env(&1)})

    try do
      Enum.each(@env_keys, &System.delete_env/1)
      Enum.each(values, fn {key, value} -> System.put_env(key, value) end)
      fun.()
    after
      Enum.each(previous, fn
        {key, nil} -> System.delete_env(key)
        {key, value} -> System.put_env(key, value)
      end)
    end
  end
end
