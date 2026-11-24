package dev.tinkr

import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent

private fun runTinkr(e: AnActionEvent, title: String, vararg args: String) {
    val output = try {
        val proc = ProcessBuilder("tinkr", *args).redirectErrorStream(true).start()
        proc.inputStream.bufferedReader().readText()
    } catch (ex: Exception) {
        "tinkr not found on PATH: ${ex.message}"
    }
    NotificationGroupManager.getInstance()
        .getNotificationGroup("Tinkr")
        ?.createNotification(title, output.take(800), NotificationType.INFORMATION)
        ?.notify(e.project)
}

class TinkrDoctorAction : AnAction("Tinkr: Doctor") {
    override fun actionPerformed(e: AnActionEvent) = runTinkr(e, "Tinkr Doctor", "doctor", "--fix")
}

class TinkrGainAction : AnAction("Tinkr: Gain") {
    override fun actionPerformed(e: AnActionEvent) = runTinkr(e, "Tinkr Gain", "gain")
}
