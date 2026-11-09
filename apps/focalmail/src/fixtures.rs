use crate::Message;
pub fn messages() -> Vec<Message> {
    let samples = [
            ("Bob Dylan", "bob@example.com", "Long time no see!", "Hi Alice,\n\nHope you're doing well! About our picnic — we'll be meeting at the park around 2PM. Looking forward to seeing you!\n\nBest regards,\n\nBob", "5:23 PM", "Gmail"),
            ("FocalDesk Team", "team@example.com", "Welcome to FocalMail", "Your mail, at home on FocalDesk.\n\nThis preview explores a familiar, focused workspace for your Gmail and Microsoft 365 accounts. All mail shown here is sample data.", "4:10 PM", "Microsoft 365"),
            ("Cristine", "cristine@example.com", "Invitation: Next release meeting", "Hi Alice,\n\nCan we review the next release on Tuesday at 1PM?\n\nThanks,\nCristine", "3:42 PM", "Microsoft 365"),
            ("Vox's Today Explained", "news@example.com", "The shape of things to come", "Your daily briefing.\n\nToday we're exploring the ideas shaping tomorrow's cities, communities, and technology.", "12:24 PM", "Gmail"),
            ("Ann Handley", "ann@example.com", "A few things worth sharing", "Hello!\n\nThree examples of creativity in unexpected places. A little inspiration for your afternoon.", "11:08 AM", "Gmail"),
            ("The Marginalian", "letters@example.com", "The art of paying attention", "A Sunday letter about curiosity, creativity, and the quiet beauty of everyday life.", "Yesterday", "Gmail"),
            ("New Yorker Science & Tech", "science@example.com", "What is the opposite of oil drilling?", "This week's science and technology reading list.\n\nA closer look at the changing world around us.", "Yesterday", "Gmail"),
        ];
    samples
        .into_iter()
        .enumerate()
        .map(
            |(i, (sender, email, subject, body, date, account))| Message {
                id: format!("demo-{i}"),
                sender: sender.into(),
                email: email.into(),
                subject: subject.into(),
                body: body.into(),
                date: date.into(),
                account: account.into(),
                folder: "Inbox".into(),
                unread: i != 0,
                flagged: false,
                labels: Vec::new(),
                timestamp: 0,
                attachments: Vec::new(),
                attachments_loaded: false,
                html: None,
                html_loaded: false,
                to_header: String::new(),
                cc_header: String::new(),
                reply_to: String::new(),
                delivered_to: String::new(),
                message_id: String::new(),
                references: String::new(),
                thread_id: String::new(),
                recipients_loaded: true,
            },
        )
        .collect()
}
