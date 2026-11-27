// SPDX-License-Identifier: GPL-3.0-or-later
#include "frame_log.h"

#include "util/u_logging.h"

#include <cstdlib>
#include <format>

namespace wivrn
{

frame_log::frame_log()
{
	const char * path = std::getenv("WIVRN_FRAME_LOG");
	if (not path or not *path)
		return;
	file.open(path, std::ios::app);
	if (file)
		U_LOG_I("Writing the frame log to %s", path);
	else
		U_LOG_E("Cannot open the frame log %s", path);
}

void frame_log::write(const std::string & line)
{
	std::lock_guard lock(mutex);
	file << line << '\n';
}

void frame_log::sent(uint8_t stream, uint64_t frame, const to_headset::video_frame_info::timing_info_t & timing, size_t bytes)
{
	if (not *this)
		return;
	write(std::format(R"({{"type":"sent","stream":{},"frame":{},"encode_begin":{},"encode_end":{},"send_begin":{},"send_end":{},"bytes":{}}})",
	                  stream,
	                  frame,
	                  timing.encode_begin,
	                  timing.encode_end,
	                  timing.send_begin,
	                  timing.send_end,
	                  bytes));
}

void frame_log::feedback(const from_headset::feedback & f)
{
	if (not *this)
		return;
	write(std::format(R"({{"type":"feedback","generation":{},"stream":{},"frame":{},"encode_begin":{},"send_end":{},"received_first_packet":{},"received_last_packet":{},"received_from_decoder":{},"blitted":{},"displayed":{},"times_displayed":{},"fec_recovered":{},"partial":{},"resent":{}}})",
	                  f.generation,
	                  f.stream_index,
	                  f.frame_index,
	                  f.encode_begin,
	                  f.send_end,
	                  f.received_first_packet,
	                  f.received_last_packet,
	                  f.received_from_decoder,
	                  f.blitted,
	                  f.displayed,
	                  f.times_displayed,
	                  f.fec_recovered,
	                  f.partial,
	                  f.resent));
}

void frame_log::network(XrTime now, const to_headset::network_report & report)
{
	if (not *this)
		return;
	std::string links;
	for (const auto & link: report.links)
		links += std::format(R"({}{{"local":"{}","remote":"{}","capacity_bps":{},"sent_bps":{},"acknowledgement_loss":{}}})",
		                     links.empty() ? "" : ",",
		                     link.local_interface,
		                     link.remote_interface,
		                     link.capacity_bps,
		                     link.sent_bps,
		                     link.acknowledgement_loss);
	write(std::format(R"({{"type":"network","time":{},"multilink":{},"bitrate":{},"late_frames":{},"expired_video":{},"links":[{}]}})",
	                  now,
	                  report.multilink,
	                  report.effective_bitrate,
	                  report.late_frames,
	                  report.expired_video,
	                  links));
}

} // namespace wivrn
