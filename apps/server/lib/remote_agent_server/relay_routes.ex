defmodule RemoteAgentServer.RelayRoutes do
  @moduledoc """
  Ephemeral, point-to-point byte routes. Client IDs are assigned by the relay,
  never accepted from a client. Application authentication happens end to end.
  """
  use GenServer

  @max_runners 128
  @max_clients_per_runner 64
  @max_pending_messages 128

  def start_link(_options), do: GenServer.start_link(__MODULE__, %{}, name: __MODULE__)

  def join(runner_id, role), do: GenServer.call(__MODULE__, {:join, runner_id, role})
  def forward(client_id, data), do: GenServer.call(__MODULE__, {:forward, client_id, data})
  def close_client(client_id), do: GenServer.call(__MODULE__, {:close_client, client_id})
  def leave, do: GenServer.call(__MODULE__, :leave)

  @impl true
  def init(_options), do: {:ok, %{runners: %{}, members: %{}, monitors: %{}}}

  @impl true
  def handle_call({:join, runner_id, "runner"}, {pid, _}, state) do
    cond do
      Map.has_key?(state.members, pid) ->
        {:reply, {:error, "already_joined"}, state}

      Map.has_key?(state.runners, runner_id) ->
        {:reply, {:error, "runner_already_connected"}, state}

      map_size(state.runners) >= @max_runners ->
        {:reply, {:error, "capacity_reached"}, state}

      true ->
        runner = %{pid: pid, clients: %{}}
        state = put_in(state.runners[runner_id], runner)
        {:reply, {:ok, nil}, monitor_member(state, pid, {:runner, runner_id})}
    end
  end

  def handle_call({:join, runner_id, "mobile"}, {pid, _}, state) do
    case Map.fetch(state.runners, runner_id) do
      {:ok, runner} when map_size(runner.clients) < @max_clients_per_runner ->
        if Map.has_key?(state.members, pid) do
          {:reply, {:error, "already_joined"}, state}
        else
          join_mobile(state, runner_id, runner.pid, pid)
        end

      {:ok, _runner} ->
        {:reply, {:error, "capacity_reached"}, state}

      :error ->
        {:reply, {:error, "runner_offline"}, state}
    end
  end

  def handle_call({:forward, client_id, data}, {pid, _}, state) do
    result =
      case Map.get(state.members, pid) do
        {:runner, runner_id} when is_binary(client_id) ->
          case get_in(state.runners, [runner_id, :clients, client_id]) do
            nil -> {:error, "client_closed"}
            target -> deliver(target, data_message(client_id, data))
          end

        {:mobile, runner_id, assigned_id} when is_nil(client_id) ->
          case Map.get(state.runners, runner_id) do
            nil -> {:error, "runner_offline"}
            runner -> deliver(runner.pid, data_message(assigned_id, data))
          end

        _ ->
          {:error, "unauthorized"}
      end

    {:reply, result, state}
  end

  def handle_call({:close_client, client_id}, {pid, _}, state) do
    case Map.get(state.members, pid) do
      {:runner, runner_id} ->
        case get_in(state.runners, [runner_id, :clients, client_id]) do
          nil ->
            {:reply, :ok, state}

          client_pid ->
            send(client_pid, {:relay_closed, "connection_closed"})
            {:reply, :ok, remove_member(state, client_pid)}
        end

      _ ->
        {:reply, {:error, "unauthorized"}, state}
    end
  end

  def handle_call(:leave, {pid, _}, state), do: {:reply, :ok, remove_member(state, pid)}

  @impl true
  def handle_info({:DOWN, ref, :process, _pid, _reason}, state) do
    case Map.pop(state.monitors, ref) do
      {nil, _} -> {:noreply, state}
      {pid, monitors} -> {:noreply, remove_member(%{state | monitors: monitors}, pid)}
    end
  end

  defp join_mobile(state, runner_id, runner_pid, pid) do
    client_id = :crypto.strong_rand_bytes(16) |> Base.url_encode64(padding: false)

    case deliver(runner_pid, {:relay_open, client_id}) do
      :ok ->
        state = put_in(state.runners[runner_id].clients[client_id], pid)
        state = monitor_member(state, pid, {:mobile, runner_id, client_id})
        {:reply, {:ok, client_id}, state}

      {:error, reason} ->
        {:reply, {:error, reason}, state}
    end
  end

  defp monitor_member(state, pid, member) do
    ref = Process.monitor(pid)

    %{
      state
      | members: Map.put(state.members, pid, member),
        monitors: Map.put(state.monitors, ref, pid)
    }
  end

  defp remove_member(state, pid) do
    {member, members} = Map.pop(state.members, pid)
    state = %{state | members: members, monitors: remove_monitor(state.monitors, pid)}

    case member do
      {:runner, runner_id} ->
        {runner, runners} = Map.pop(state.runners, runner_id)
        state = %{state | runners: runners}

        Enum.reduce(runner.clients, state, fn {_id, client_pid}, acc ->
          send(client_pid, {:relay_closed, "runner_offline"})
          remove_member(acc, client_pid)
        end)

      {:mobile, runner_id, client_id} ->
        case Map.get(state.runners, runner_id) do
          nil ->
            state

          runner ->
            send(runner.pid, {:relay_close, client_id})
            put_in(state.runners[runner_id].clients, Map.delete(runner.clients, client_id))
        end

      nil ->
        state
    end
  end

  defp remove_monitor(monitors, pid) do
    Enum.reduce(monitors, monitors, fn
      {ref, ^pid}, acc ->
        Process.demonitor(ref, [:flush])
        Map.delete(acc, ref)

      _, acc ->
        acc
    end)
  end

  defp deliver(pid, message) do
    case Process.info(pid, :message_queue_len) do
      {:message_queue_len, count} when count < @max_pending_messages ->
        send(pid, message)
        :ok

      _ ->
        {:error, "peer_unavailable"}
    end
  end

  defp data_message(id, :ack), do: {:relay_ack, id}
  defp data_message(id, data), do: {:relay_data, id, data}
end
