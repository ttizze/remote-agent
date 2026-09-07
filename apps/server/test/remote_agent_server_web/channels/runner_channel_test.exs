defmodule RemoteAgentServerWeb.RunnerChannelTest do
  use RemoteAgentServerWeb.ChannelCase, async: false

  alias RemoteAgentServerWeb.UserSocket

  @relay_token "isolated-test-relay-token"

  setup do
    Process.flag(:trap_exit, true)
    previous = Application.get_env(:remote_agent_server, :relay_token)
    Application.put_env(:remote_agent_server, :relay_token, @relay_token)
    on_exit(fn -> Application.put_env(:remote_agent_server, :relay_token, previous) end)
    {:ok, runner_id: "runner-#{System.unique_integer([:positive])}"}
  end

  test "routes bytes separately for two clients and rejects a forged client id", %{runner_id: id} do
    runner = join_role(id, "runner")
    first = join_role(id, "mobile")
    assert_push("open", %{clientId: first_id})
    second = join_role(id, "mobile")
    assert_push("open", %{clientId: second_id})
    refute first_id == second_id

    first_data = Base.encode64(<<0, 255, 17>>)
    second_data = Base.encode64(<<0, 255, 18>>)
    ref = push(first, "data", %{"data" => first_data})
    assert_reply(ref, :ok)
    assert_push("data", %{clientId: ^first_id, data: ^first_data})
    ref = push(second, "data", %{"data" => second_data})
    assert_reply(ref, :ok)
    assert_push("data", %{clientId: ^second_id, data: ^second_data})
    refute_push("data", _)

    ref = push(runner, "data", %{"clientId" => first_id, "data" => first_data})
    assert_reply(ref, :ok)
    assert_push("data", %{data: ^first_data})
    refute_push("data", _)

    ref = push(first, "ack", %{})
    assert_reply(ref, :ok)
    assert_push("ack", %{clientId: ^first_id})
    ref = push(runner, "ack", %{"clientId" => second_id})
    assert_reply(ref, :ok)
    assert_push("ack", %{})
    refute_push("ack", _)

    ref = push(first, "data", %{"clientId" => second_id, "data" => first_data})
    assert_reply(ref, :error, %{reason: "unauthorized"})
    refute_push("data", _)
  end

  test "runner cannot target clients of another runner", %{runner_id: id} do
    runner = join_role(id, "runner")
    _other_runner = join_role(id <> "-other", "runner")
    _client = join_role(id <> "-other", "mobile")
    assert_push("open", %{clientId: other_client})
    ref = push(runner, "data", %{"clientId" => other_client, "data" => Base.encode64("opaque")})
    assert_reply(ref, :error, %{reason: "client_closed"})
    refute_push("data", _)
  end

  test "rejects duplicate runners and offline joins", %{runner_id: id} do
    assert {:error, %{reason: "runner_offline"}} = connect_and_join(id, "mobile")
    _runner = join_role(id, "runner")
    assert {:error, %{reason: "runner_already_connected"}} = connect_and_join(id, "runner")
  end

  test "closed clients are removed and a fresh connection gets a fresh route", %{runner_id: id} do
    runner = join_role(id, "runner")
    first = join_role(id, "mobile")
    assert_push("open", %{clientId: first_id})
    close(first)
    assert_push("close", %{clientId: ^first_id})
    ref = push(runner, "data", %{"clientId" => first_id, "data" => Base.encode64("old")})
    assert_reply(ref, :error, %{reason: "client_closed"})
    _second = join_role(id, "mobile")
    assert_push("open", %{clientId: second_id})
    refute first_id == second_id
  end

  test "runner disappearance closes clients and releases ownership", %{runner_id: id} do
    runner = join_role(id, "runner")
    _client = join_role(id, "mobile")
    assert_push("open", _)
    close(runner)
    assert_push("closed", %{reason: "runner_offline"})
    _replacement = join_role(id, "runner")
  end

  test "runner can close one client without disconnecting the other", %{runner_id: id} do
    runner = join_role(id, "runner")
    _first = join_role(id, "mobile")
    assert_push("open", %{clientId: first_id})
    second = join_role(id, "mobile")
    assert_push("open", %{clientId: second_id})
    ref = push(runner, "close", %{"clientId" => first_id})
    assert_reply(ref, :ok)
    assert_push("closed", %{reason: "connection_closed"})
    assert_push("close", %{clientId: ^first_id})
    data = Base.encode64("still connected")
    ref = push(second, "data", %{"data" => data})
    assert_reply(ref, :ok)
    assert_push("data", %{clientId: ^second_id, data: ^data})
  end

  test "rejects plaintext, invalid base64 and oversized frames", %{runner_id: id} do
    _runner = join_role(id, "runner")
    client = join_role(id, "mobile")
    assert_push("open", _)

    for payload <- [
          %{"data" => "{}"},
          %{"data" => ""},
          %{"data" => String.duplicate("a", 65_537)}
        ] do
      ref = push(client, "data", payload)
      assert_reply(ref, :error, %{reason: "invalid_payload"})
    end

    ref = push(client, "jsonl", %{"line" => "{}"})
    assert_reply(ref, :error, %{reason: "invalid_payload"})
    refute_push("data", _)
  end

  test "socket rejects invalid credentials, role and runner id", %{runner_id: id} do
    for params <- [
          %{"runner_id" => id, "role" => "mobile"},
          %{"token" => "wrong", "runner_id" => id, "role" => "mobile"},
          %{"token" => @relay_token, "runner_id" => id, "role" => "unknown"},
          %{
            "token" => @relay_token,
            "runner_id" => String.duplicate("a", 513),
            "role" => "mobile"
          }
        ] do
      assert :error = Phoenix.ChannelTest.connect(UserSocket, params)
    end

    {:ok, socket} = connect_socket(id, "runner")
    assert {:error, %{reason: "unauthorized"}} = subscribe_and_join(socket, "runner:other")
  end

  defp join_role(id, role) do
    {:ok, _, socket} = connect_and_join(id, role)
    socket
  end

  defp connect_and_join(id, role) do
    {:ok, socket} = connect_socket(id, role)
    subscribe_and_join(socket, "runner:#{id}")
  end

  defp connect_socket(id, role) do
    Phoenix.ChannelTest.connect(UserSocket, %{
      "token" => @relay_token,
      "runner_id" => id,
      "role" => role
    })
  end
end
