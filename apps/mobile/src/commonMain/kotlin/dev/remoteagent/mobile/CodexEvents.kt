package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonObject

internal val RawCodexMessage.paramsObject: JsonObject
    get() = params.asObjectOrNull() ?: JsonObject(mapOf("value" to params))

internal val RawCodexMessage.itemId: String?
    get() =
        when (kind) {
            ConversationEventKind.ItemStarted,
            ConversationEventKind.ItemCompleted -> paramsObject.childObject("item")?.string("id")
            ConversationEventKind.GuardianReviewChanged -> paramsObject.string("reviewId")
            else -> paramsObject.string("itemId")
        }

internal fun RawCodexMessage.conversationItem(): CodexItem =
    if (kind == ConversationEventKind.GuardianReviewChanged)
        CodexItem.Unknown(paramsObject.string("reviewId").orEmpty(), "automaticApprovalReview", paramsObject)
    else codexItem(paramsObject["item"] ?: emptyJsonObject())
