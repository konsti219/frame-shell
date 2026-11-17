#include "version.h"
#include <KAboutData>
#include <KIconTheme>
#include <KLocalizedQmlContext>
#include <KLocalizedString>
#include <QApplication>
#include <QCoroQml>
#include <QCoroQmlTask>
#include <QQmlApplicationEngine>
#include <QQuickStyle>
#include <QUrl>
#include <QtQml>

#include <QQuickWindow>
#include <QStringLiteral>

int main(int argc, char * argv[])
{
	KIconTheme::initTheme();
	QApplication app(argc, argv);
	KLocalizedString::setApplicationDomain("wivrn-dashboard");
	app.setOrganizationName(QStringLiteral("wivrn"));

	KAboutData aboutData(
	        QStringLiteral("wivrn-dashboard"),
	        i18nc("@title", "Frame Shell"),
	        wivrn::display_version(),
	        i18n("Frame Shell PC host"),
	        KAboutLicense::GPL_V3,
	        i18n("(c) 2022-2026 WiVRn development team"));

	aboutData.setDesktopFileName(QStringLiteral("io.github.wivrn.wivrn"));
	aboutData.setOrganizationDomain("wivrn.github.io");

	aboutData.setHomepage("https://github.com/konsti219/frame-shell");
	aboutData.setBugAddress("https://github.com/konsti219/frame-shell/issues");
	aboutData.setProgramLogo(QIcon(":/qml/frame-shell.png"));

	aboutData.addComponent(
	        i18n("Monado"),
	        i18n("OpenXR runtime"),
	        "",
	        "https://monado.dev/",
	        KAboutLicense::BSL_V1);

	// Set aboutData as information about the app
	KAboutData::setApplicationData(aboutData);

	QCoro::Qml::registerTypes();

	// Work around QTBUG-45105, QTBUG-46074, QTBUG-51112: flicker when resizing
	QQuickWindow::setSceneGraphBackend(QString::fromLatin1("software"));

	QApplication::setStyle(QStringLiteral("breeze"));
	if (qEnvironmentVariableIsEmpty("QT_QUICK_CONTROLS_STYLE"))
	{
		QQuickStyle::setStyle(QStringLiteral("org.kde.desktop"));
	}

	QQmlApplicationEngine engine;

	KLocalization::setupLocalizedContext(&engine);
	engine.loadFromModule("io.github.wivrn.wivrn", "Main");

	if (engine.rootObjects().isEmpty())
	{
		return -1;
	}

	return app.exec();
}
