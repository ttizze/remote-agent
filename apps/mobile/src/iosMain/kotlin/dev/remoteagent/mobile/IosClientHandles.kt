package dev.remoteagent.mobile

import cnames.structs.MobileClientHandle
import kotlinx.atomicfu.locks.SynchronizedObject
import kotlinx.atomicfu.locks.synchronized
import kotlinx.cinterop.CPointer
import kotlinx.cinterop.ExperimentalForeignApi
import kotlinx.coroutines.Job
import mobile_client.mobile_client_close

/** Handle replacement, leasing, and subscription registration share this lock. */
@OptIn(ExperimentalForeignApi::class)
internal class IosClientHandles {
    private val handleLock = SynchronizedObject()
    private val handles = mutableMapOf<String, NativeHandle>()
    private val subscriptions = mutableMapOf<String, MutableSet<Job>>()

    fun replace(hostIdentity: String, pointer: CPointer<MobileClientHandle>) {
        val (jobs, retired) =
            synchronized(handleLock) {
                val jobs = subscriptions.remove(hostIdentity)?.toList().orEmpty()
                val previous = handles[hostIdentity]
                previous?.retired = true
                handles[hostIdentity] = NativeHandle(pointer)
                jobs to previous?.pointer?.takeIf { previous.borrowers == 0 }
            }
        jobs.forEach(Job::cancel)
        retired?.let { mobile_client_close(it) }
    }

    fun retire(hostIdentity: String) {
        val (jobs, retired) =
            synchronized(handleLock) {
                val jobs = subscriptions.remove(hostIdentity)?.toList().orEmpty()
                val pointer =
                    handles
                        .remove(hostIdentity)
                        ?.also { it.retired = true }
                        ?.let { handle -> handle.pointer.takeIf { handle.borrowers == 0 } }
                jobs to pointer
            }
        jobs.forEach(Job::cancel)
        retired?.let { mobile_client_close(it) }
    }

    fun register(hostIdentity: String, create: (NativeHandle) -> Job): Job? =
        synchronized(handleLock) {
            val handle = handles[hostIdentity] ?: return@synchronized null
            val job = create(handle)
            subscriptions.getOrPut(hostIdentity) { mutableSetOf() }.add(job)
            job
        }

    fun <T> withHandle(
        hostIdentity: String,
        expectedHandle: NativeHandle? = null,
        block: (CPointer<MobileClientHandle>) -> T,
    ): T? {
        val lease =
            synchronized(handleLock) {
                handles[hostIdentity]
                    ?.takeIf { expectedHandle == null || it === expectedHandle }
                    ?.also { it.borrowers += 1 }
            } ?: return null
        return try {
            block(lease.pointer)
        } finally {
            val retired =
                synchronized(handleLock) {
                    lease.borrowers -= 1
                    lease.pointer.takeIf { lease.retired && lease.borrowers == 0 }
                }
            retired?.let { mobile_client_close(it) }
        }
    }

    fun isCurrentHandle(hostIdentity: String, expectedHandle: NativeHandle): Boolean =
        synchronized(handleLock) { handles[hostIdentity] === expectedHandle }

    fun removeSubscription(hostIdentity: String, job: Job) {
        synchronized(handleLock) {
            subscriptions[hostIdentity]?.let { jobs ->
                jobs.remove(job)
                if (jobs.isEmpty()) subscriptions.remove(hostIdentity)
            }
        }
    }

    class NativeHandle(val pointer: CPointer<MobileClientHandle>, var borrowers: Int = 0, var retired: Boolean = false)
}
