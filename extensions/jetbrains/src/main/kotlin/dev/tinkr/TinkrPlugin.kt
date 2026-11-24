package dev.tinkr

import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent

class TinkrDoctorAction : AnAction("Tinkr: Doctor") {
    override fun actionPerformed(e: AnActionEvent) {
        val output = try {
            val proc = ProcessBuilder("tinkr", "doctor", "--fix").redirectErrorStream(true).start()
            proc.inputStream.bufferedReader().readText()
        } catch (ex: Exception) {
            "tinkr not found on PATH: ${ex.message}"
        }
        NotificationGroupManager.getInstance()
            .getNotificationGroup("Tinkr")
            ?.createNotification("Tinkr Doctor", output.take(500), NotificationType.INFORMATION)
            ?.notify(e.project)
    }
}
