[00:00:01] What<00:00:00.200> is<00:00:00.280> going<00:00:00.480> on,<00:00:00.640> guys?<00:00:00.920> Welcome<00:00:01.320> back. What is going on, guys? Welcome back.
[00:00:01] What is going on, guys? Welcome back. Today,<00:00:01.800> we're<00:00:01.920> going<00:00:02.120> to<00:00:02.240> learn<00:00:02.480> about<00:00:02.880> Git
[00:00:03] Today, we're going to learn about Git
[00:00:03] Today, we're going to learn about Git work<00:00:03.480> trees,<00:00:03.760> and<00:00:03.880> specifically,<00:00:04.520> we're
[00:00:04] work trees, and specifically, we're
[00:00:04] work trees, and specifically, we're going<00:00:04.800> to<00:00:04.880> learn<00:00:05.080> about<00:00:05.360> them<00:00:05.560> in<00:00:05.720> the<00:00:05.840> context
[00:00:06] going to learn about them in the context
[00:00:06] going to learn about them in the context of<00:00:06.560> agentic<00:00:07.040> programming.<00:00:07.600> So,<00:00:07.760> we're<00:00:07.840> going
[00:00:08] of agentic programming. So, we're going
[00:00:08] of agentic programming. So, we're going to<00:00:08.120> briefly<00:00:08.440> discuss<00:00:08.800> the<00:00:08.920> general<00:00:09.400> idea
[00:00:09] to briefly discuss the general idea
[00:00:09] to briefly discuss the general idea behind<00:00:10.040> Git<00:00:10.200> work<00:00:10.440> trees,<00:00:10.920> and<00:00:11.040> then<00:00:11.160> we're
[00:00:11] behind Git work trees, and then we're
[00:00:11] behind Git work trees, and then we're going<00:00:11.400> to<00:00:11.520> see<00:00:11.720> how<00:00:11.960> we<00:00:12.080> can<00:00:12.240> use<00:00:12.440> that<00:00:12.720> to<00:00:12.880> run
[00:00:13] going to see how we can use that to run
[00:00:13] going to see how we can use that to run multiple<00:00:13.560> Cloud<00:00:13.960> Code<00:00:14.320> or<00:00:14.400> whatever<00:00:14.840> agentic
[00:00:15] multiple Cloud Code or whatever agentic
[00:00:15] multiple Cloud Code or whatever agentic code<00:00:16.240> or<00:00:16.400> environment<00:00:16.960> you're<00:00:17.080> using<00:00:17.880> in
[00:00:18] code or environment you're using in
[00:00:18] code or environment you're using in parallel.<00:00:18.600> So,<00:00:18.760> many<00:00:19.080> sessions<00:00:19.480> in<00:00:19.560> parallel
[00:00:19] parallel. So, many sessions in parallel
[00:00:19] parallel. So, many sessions in parallel working<00:00:20.440> on<00:00:20.640> specific<00:00:21.160> parts<00:00:21.480> of<00:00:21.600> the<00:00:21.680> same
[00:00:22] working on specific parts of the same
[00:00:22] working on specific parts of the same project,<00:00:22.840> same<00:00:22.960> application,<00:00:24.000> but<00:00:24.160> doing<00:00:24.480> so
[00:00:24] project, same application, but doing so
[00:00:25] project, same application, but doing so in<00:00:25.120> multiple<00:00:25.480> instances.<00:00:26.240> If<00:00:26.360> you<00:00:26.440> like<00:00:26.600> this
[00:00:26] in multiple instances. If you like this
[00:00:26] in multiple instances. If you like this video,<00:00:26.960> let<00:00:27.120> me<00:00:27.200> know<00:00:27.320> by<00:00:27.480> hitting<00:00:27.720> the<00:00:27.800> like
[00:00:27] video, let me know by hitting the like
[00:00:27] video, let me know by hitting the like button<00:00:28.160> and<00:00:28.240> subscribing,<00:00:28.880> but<00:00:29.080> now<00:00:29.600> let<00:00:29.720> us
[00:00:29] button and subscribing, but now let us
[00:00:29] button and subscribing, but now let us get<00:00:29.920> right<00:00:30.040> into<00:00:30.320> it.
[00:00:39] &gt;&gt; [music] &gt;&gt; [music]
[00:00:39] &gt;&gt; [music] &gt;&gt; All<00:00:39.800> right,<00:00:40.000> so<00:00:40.120> we're<00:00:40.200> going<00:00:40.360> to<00:00:40.400> keep<00:00:40.600> it
[00:00:40] &gt;&gt; All right, so we're going to keep it
[00:00:40] &gt;&gt; All right, so we're going to keep it straightforward<00:00:41.280> and<00:00:41.440> simple.<00:00:41.880> We're<00:00:41.960> going
[00:00:42] straightforward and simple. We're going
[00:00:42] straightforward and simple. We're going to<00:00:42.240> discuss<00:00:42.640> briefly<00:00:43.040> what<00:00:43.240> Git<00:00:43.440> work<00:00:43.680> trees
[00:00:43] to discuss briefly what Git work trees
[00:00:43] to discuss briefly what Git work trees are<00:00:44.080> in<00:00:44.240> general,<00:00:44.640> what<00:00:44.800> the<00:00:44.920> use<00:00:45.160> case<00:00:45.400> for
[00:00:45] are in general, what the use case for
[00:00:45] are in general, what the use case for Git<00:00:45.760> work<00:00:45.960> trees<00:00:46.200> is,<00:00:46.360> and<00:00:46.480> then<00:00:46.600> we're<00:00:46.680> going
[00:00:46] Git work trees is, and then we're going
[00:00:46] Git work trees is, and then we're going to<00:00:47.000> see<00:00:47.240> how<00:00:47.480> this<00:00:47.680> applies<00:00:48.160> to<00:00:48.360> agentic
[00:00:48] to see how this applies to agentic
[00:00:48] to see how this applies to agentic coding,<00:00:49.280> how<00:00:49.440> we<00:00:49.560> can<00:00:49.680> use<00:00:49.880> multiple
[00:00:50] coding, how we can use multiple
[00:00:50] coding, how we can use multiple instances<00:00:51.360> of<00:00:51.520> coding<00:00:51.920> agents<00:00:52.360> to<00:00:52.480> work<00:00:52.720> on
[00:00:52] instances of coding agents to work on
[00:00:52] instances of coding agents to work on the<00:00:52.920> same<00:00:53.160> project<00:00:53.640> without<00:00:54.120> any<00:00:54.800> problems,
[00:00:55] the same project without any problems,
[00:00:55] the same project without any problems, without<00:00:55.680> any<00:00:56.080> inconsistency,<00:00:57.040> so<00:00:57.200> to<00:00:57.320> say.
[00:00:57] without any inconsistency, so to say.
[00:00:58] without any inconsistency, so to say. So,<00:00:58.280> for<00:00:58.440> this<00:00:58.600> very<00:00:58.800> basic<00:00:59.080> example<00:00:59.480> here,<00:00:59.680> I
[00:00:59] So, for this very basic example here, I
[00:00:59] So, for this very basic example here, I have<00:01:00.120> a<00:01:00.200> Git<00:01:00.440> repository<00:01:01.080> called<00:01:01.400> work<00:01:01.640> tree
[00:01:01] have a Git repository called work tree
[00:01:01] have a Git repository called work tree tutorial.<00:01:02.320> It<00:01:02.440> contains<00:01:02.960> a<00:01:03.080> vibe<00:01:03.400> coded<00:01:03.760> Flask
[00:01:04] tutorial. It contains a vibe coded Flask
[00:01:04] tutorial. It contains a vibe coded Flask app.<00:01:04.680> So,<00:01:04.839> what<00:01:04.960> I<00:01:05.040> can<00:01:05.199> do<00:01:05.320> is<00:01:05.440> I<00:01:05.519> can<00:01:05.680> clone<00:01:06.000> it
[00:01:06] app. So, what I can do is I can clone it
[00:01:06] app. So, what I can do is I can clone it here.<00:01:06.440> I<00:01:06.520> already<00:01:06.880> have<00:01:07.120> this<00:01:07.280> on<00:01:07.440> my<00:01:07.560> system.
[00:01:08] here. I already have this on my system.
[00:01:08] here. I already have this on my system. To<00:01:08.560> be<00:01:08.640> precise,<00:01:09.120> it's<00:01:09.240> in<00:01:09.360> my<00:01:09.480> tutorial
[00:01:09] To be precise, it's in my tutorial
[00:01:09] To be precise, it's in my tutorial directory<00:01:10.440> here.<00:01:11.280> And<00:01:11.640> the<00:01:11.760> basic<00:01:12.080> idea<00:01:12.440> is<00:01:12.600> I
[00:01:12] directory here. And the basic idea is I
[00:01:12] directory here. And the basic idea is I have<00:01:12.880> this<00:01:13.040> repository<00:01:13.640> now.<00:01:14.040> I<00:01:14.160> go<00:01:14.440> into<00:01:14.800> it.
[00:01:15] have this repository now. I go into it.
[00:01:15] have this repository now. I go into it. We<00:01:15.640> can<00:01:15.800> also<00:01:16.080> run<00:01:16.240> this<00:01:16.440> if<00:01:16.560> we<00:01:16.680> want<00:01:16.920> to,<00:01:17.000> so<00:01:17.120> I
[00:01:17] We can also run this if we want to, so I
[00:01:17] We can also run this if we want to, so I can<00:01:17.320> say
[00:01:18] can say
[00:01:18] can say in<00:01:18.720> this<00:01:18.920> Flask<00:01:19.240> to<00:01:19.360> do<00:01:19.560> app,<00:01:19.760> it's<00:01:19.880> a<00:01:19.960> UV
[00:01:20] in this Flask to do app, it's a UV
[00:01:20] in this Flask to do app, it's a UV application,<00:01:20.920> so<00:01:21.000> I<00:01:21.040> can<00:01:21.200> say<00:01:21.360> UV<00:01:21.640> run
[00:01:22] application, so I can say UV run
[00:01:22] application, so I can say UV run main.py.<00:01:23.160> This<00:01:23.360> is<00:01:23.440> going<00:01:23.640> to<00:01:23.720> install
[00:01:24] main.py. This is going to install
[00:01:24] main.py. This is going to install everything.<00:01:24.600> It's<00:01:24.760> going<00:01:25.000> to<00:01:25.080> allow<00:01:25.320> me<00:01:25.480> to
[00:01:25] everything. It's going to allow me to
[00:01:25] everything. It's going to allow me to run<00:01:25.840> this<00:01:26.040> on<00:01:26.160> localhost,<00:01:26.760> and<00:01:26.920> it's<00:01:27.040> a<00:01:27.120> basic
[00:01:27] run this on localhost, and it's a basic
[00:01:27] run this on localhost, and it's a basic to<00:01:27.640> do<00:01:27.800> application.<00:01:28.440> So,<00:01:28.560> I<00:01:28.600> can<00:01:28.760> create<00:01:29.000> a
[00:01:29] to do application. So, I can create a
[00:01:29] to do application. So, I can create a user,<00:01:29.400> I<00:01:29.480> can<00:01:29.680> log<00:01:29.920> in,<00:01:30.120> I<00:01:30.200> can
[00:01:31] user, I can log in, I can
[00:01:31] user, I can log in, I can manage<00:01:31.680> to<00:01:31.800> do's,<00:01:32.160> and<00:01:32.280> you<00:01:32.360> can<00:01:32.480> see<00:01:32.640> it<00:01:32.760> has<00:01:33.040> a
[00:01:33] manage to do's, and you can see it has a
[00:01:33] manage to do's, and you can see it has a bluish<00:01:33.960> kind<00:01:34.160> of<00:01:34.280> theme<00:01:34.560> here.
[00:01:35] bluish kind of theme here.
[00:01:35] bluish kind of theme here. Now,<00:01:36.240> the<00:01:36.400> application<00:01:36.960> we're<00:01:37.080> working<00:01:37.360> with
[00:01:37] Now, the application we're working with
[00:01:37] Now, the application we're working with here<00:01:37.640> is<00:01:37.720> not<00:01:37.920> that<00:01:38.080> important,<00:01:38.560> but<00:01:38.680> it's<00:01:38.800> a
[00:01:38] here is not that important, but it's a
[00:01:38] here is not that important, but it's a Flask<00:01:39.200> application.<00:01:39.800> We<00:01:39.920> can<00:01:40.040> change
[00:01:40] Flask application. We can change
[00:01:40] Flask application. We can change something,<00:01:40.720> so<00:01:40.880> maybe<00:01:41.400> I<00:01:41.560> want<00:01:41.760> to<00:01:41.800> start
[00:01:42] something, so maybe I want to start
[00:01:42] something, so maybe I want to start developing<00:01:42.720> a<00:01:42.800> new<00:01:43.000> feature<00:01:43.360> now.<00:01:43.600> I<00:01:43.640> want<00:01:43.800> to
[00:01:43] developing a new feature now. I want to
[00:01:43] developing a new feature now. I want to change<00:01:44.160> something<00:01:44.440> about<00:01:44.720> the<00:01:44.800> code.<00:01:45.480> So,<00:01:45.560> I
[00:01:45] change something about the code. So, I
[00:01:45] change something about the code. So, I can<00:01:45.760> go<00:01:45.920> into<00:01:46.160> my<00:01:46.320> to<00:01:46.520> do<00:01:46.680> blueprint<00:01:47.160> here.<00:01:47.400> I
[00:01:47] can go into my to do blueprint here. I
[00:01:47] can go into my to do blueprint here. I can<00:01:47.600> go<00:01:47.760> into<00:01:47.920> the<00:01:48.040> routes<00:01:48.880> and<00:01:49.160> adjust
[00:01:49] can go into the routes and adjust
[00:01:49] can go into the routes and adjust something<00:01:49.960> in<00:01:50.080> the<00:01:50.200> code.<00:01:50.640> Maybe<00:01:50.960> I<00:01:51.040> want<00:01:51.320> to
[00:01:51] something in the code. Maybe I want to
[00:01:51] something in the code. Maybe I want to say<00:01:52.080> that<00:01:52.200> there<00:01:52.320> can<00:01:52.600> be<00:01:52.800> another<00:01:53.200> filter
[00:01:53] say that there can be another filter
[00:01:53] say that there can be another filter status.<00:01:54.000> So,<00:01:54.120> I<00:01:54.160> can<00:01:54.360> say<00:01:55.160> filter<00:01:55.440> status<00:01:55.840> is
[00:01:55] status. So, I can say filter status is
[00:01:56] status. So, I can say filter status is equal<00:01:56.320> to
[00:01:57] equal to
[00:01:57] equal to whatever,<00:01:58.160> then<00:01:58.440> something<00:01:58.800> else<00:01:59.000> should<00:01:59.200> be
[00:02:00] whatever, then something else should be
[00:02:00] whatever, then something else should be done<00:02:00.760> here.<00:02:00.960> So,<00:02:01.040> I<00:02:01.080> can<00:02:01.240> say<00:02:01.400> query<00:02:01.880> is<00:02:02.040> equal
[00:02:02] done here. So, I can say query is equal
[00:02:02] done here. So, I can say query is equal to<00:02:02.520> query<00:02:03.680> uh<00:02:03.920> whatever.<00:02:04.240> I<00:02:04.280> don't<00:02:04.440> know<00:02:04.680> even
[00:02:04] to query uh whatever. I don't know even
[00:02:04] to query uh whatever. I don't know even know<00:02:04.960> what<00:02:05.080> I'm<00:02:05.200> doing<00:02:05.400> here.<00:02:06.000> But,<00:02:06.560> the<00:02:06.680> basic
[00:02:06] know what I'm doing here. But, the basic
[00:02:06] know what I'm doing here. But, the basic idea<00:02:07.200> is<00:02:07.360> I'm<00:02:07.560> coding<00:02:07.920> now<00:02:08.160> on<00:02:08.360> this<00:02:08.520> file.<00:02:08.800> I'm
[00:02:08] idea is I'm coding now on this file. I'm
[00:02:08] idea is I'm coding now on this file. I'm coding<00:02:09.160> on<00:02:09.240> multiple<00:02:09.679> files.<00:02:09.960> I'm<00:02:10.080> changing<00:02:10.399> a
[00:02:10] coding on multiple files. I'm changing a
[00:02:10] coding on multiple files. I'm changing a lot<00:02:10.679> in<00:02:10.759> this<00:02:10.920> project,<00:02:11.400> and<00:02:11.480> then<00:02:11.680> all<00:02:11.800> of<00:02:11.920> a
[00:02:11] lot in this project, and then all of a
[00:02:11] lot in this project, and then all of a sudden<00:02:12.240> someone,<00:02:12.800> maybe<00:02:13.000> a<00:02:13.080> client,<00:02:13.640> maybe
[00:02:13] sudden someone, maybe a client, maybe
[00:02:13] sudden someone, maybe a client, maybe some<00:02:14.200> business<00:02:14.520> partner,<00:02:14.920> maybe<00:02:15.160> someone
[00:02:15] some business partner, maybe someone
[00:02:15] some business partner, maybe someone else<00:02:15.640> here,<00:02:16.320> tells<00:02:16.600> me,<00:02:16.840> "Hey,<00:02:17.280> this<00:02:17.560> is<00:02:17.680> an
[00:02:17] else here, tells me, "Hey, this is an
[00:02:17] else here, tells me, "Hey, this is an urgent<00:02:18.240> bug.<00:02:18.520> This<00:02:18.680> is<00:02:18.800> an<00:02:18.960> urgent<00:02:19.720> problem
[00:02:20] urgent bug. This is an urgent problem
[00:02:20] urgent bug. This is an urgent problem that<00:02:20.360> is<00:02:20.480> in<00:02:20.600> the<00:02:20.720> application<00:02:21.320> right<00:02:21.520> now.
[00:02:21] that is in the application right now.
[00:02:21] that is in the application right now. Please<00:02:22.120> fix<00:02:22.360> this<00:02:22.680> ASAP."<00:02:23.240> And<00:02:23.520> let's<00:02:23.760> say
[00:02:23] Please fix this ASAP." And let's say
[00:02:23] Please fix this ASAP." And let's say it's<00:02:24.080> something<00:02:24.480> in<00:02:24.560> another<00:02:24.880> file<00:02:25.240> or<00:02:25.360> maybe
[00:02:25] it's something in another file or maybe
[00:02:25] it's something in another file or maybe in<00:02:25.680> this<00:02:25.880> file<00:02:26.080> in<00:02:26.160> a<00:02:26.200> different<00:02:26.560> function.
[00:02:27] in this file in a different function.
[00:02:27] in this file in a different function. What<00:02:27.680> do<00:02:27.840> I<00:02:27.920> do<00:02:28.120> in<00:02:28.240> this<00:02:28.440> case?<00:02:29.200> Now,<00:02:29.560> I<00:02:29.600> don't
[00:02:29] What do I do in this case? Now, I don't
[00:02:29] What do I do in this case? Now, I don't want<00:02:29.880> to<00:02:29.960> lose<00:02:30.160> all<00:02:30.280> the<00:02:30.400> progress<00:02:31.160> that<00:02:31.520> I
[00:02:31] want to lose all the progress that I
[00:02:31] want to lose all the progress that I have<00:02:31.760> already<00:02:32.080> made.<00:02:32.320> So,<00:02:32.440> I<00:02:32.480> don't<00:02:32.680> want<00:02:32.800> to
[00:02:32] have already made. So, I don't want to
[00:02:32] have already made. So, I don't want to stop<00:02:33.440> and<00:02:33.880> just<00:02:34.400> discard<00:02:34.920> everything<00:02:35.280> that
[00:02:35] stop and just discard everything that
[00:02:35] stop and just discard everything that I've<00:02:35.600> written<00:02:35.840> here.<00:02:36.040> Maybe<00:02:36.280> I've<00:02:36.480> changed
[00:02:36] I've written here. Maybe I've changed
[00:02:36] I've written here. Maybe I've changed thousands<00:02:37.320> of<00:02:37.480> lines,<00:02:37.840> hundreds<00:02:38.120> of<00:02:38.280> lines,
[00:02:38] thousands of lines, hundreds of lines,
[00:02:38] thousands of lines, hundreds of lines, especially<00:02:39.200> if<00:02:39.320> I<00:02:39.400> use<00:02:39.600> agentic<00:02:40.000> coding,
[00:02:40] especially if I use agentic coding,
[00:02:40] especially if I use agentic coding, maybe<00:02:40.520> I<00:02:40.600> changed<00:02:40.920> a<00:02:40.960> lot,<00:02:41.720> and<00:02:41.840> I<00:02:41.880> don't<00:02:42.120> want
[00:02:42] maybe I changed a lot, and I don't want
[00:02:42] maybe I changed a lot, and I don't want to<00:02:42.400> just<00:02:42.640> stash<00:02:43.040> all<00:02:43.200> of<00:02:43.320> it<00:02:43.520> and<00:02:43.920> get<00:02:44.080> it<00:02:44.200> back.
[00:02:44] to just stash all of it and get it back.
[00:02:44] to just stash all of it and get it back. I<00:02:44.600> don't<00:02:44.840> also<00:02:45.080> want<00:02:45.320> to<00:02:45.400> just<00:02:45.640> get<00:02:45.840> rid<00:02:46.000> of<00:02:46.160> all
[00:02:46] I don't also want to just get rid of all
[00:02:46] I don't also want to just get rid of all of<00:02:46.440> it.<00:02:47.000> So,<00:02:47.120> what<00:02:47.240> I'm<00:02:47.320> going<00:02:47.440> to<00:02:47.520> do<00:02:47.800> is<00:02:48.120> I'm
[00:02:48] of it. So, what I'm going to do is I'm
[00:02:48] of it. So, what I'm going to do is I'm going<00:02:48.360> to<00:02:48.440> work<00:02:48.960> with<00:02:49.240> a<00:02:49.360> work<00:02:49.640> tree.<00:02:50.000> This<00:02:50.200> is
[00:02:50] going to work with a work tree. This is
[00:02:50] going to work with a work tree. This is the<00:02:50.360> perfect<00:02:50.760> use<00:02:50.880> case<00:02:51.120> for<00:02:51.200> that.<00:02:51.840> So,<00:02:51.960> let's
[00:02:52] the perfect use case for that. So, let's
[00:02:52] the perfect use case for that. So, let's say<00:02:52.360> here<00:02:52.560> I'm<00:02:52.640> going<00:02:52.760> to<00:02:52.840> add<00:02:53.000> a<00:02:53.080> pass
[00:02:53] say here I'm going to add a pass
[00:02:53] say here I'm going to add a pass statement,<00:02:54.120> then<00:02:54.360> maybe<00:02:54.840> down<00:02:55.120> here<00:02:55.400> I<00:02:55.440> want
[00:02:55] statement, then maybe down here I want
[00:02:55] statement, then maybe down here I want to<00:02:55.880> do<00:02:56.480> Maybe<00:02:56.680> I<00:02:56.720> want<00:02:56.840> to<00:02:56.880> go<00:02:57.000> into<00:02:57.160> the<00:02:57.240> models
[00:02:57] to do Maybe I want to go into the models
[00:02:57] to do Maybe I want to go into the models here.<00:02:58.040> I<00:02:58.080> want<00:02:58.240> to<00:02:58.320> say<00:02:58.600> that<00:02:59.000> [[every]]<00:03:00.160> user<00:03:00.720> will
[00:03:00] here. I want to say that every user will
[00:03:00] here. I want to say that every user will also<00:03:01.200> have<00:03:02.400> um<00:03:02.720> I<00:03:02.760> don't<00:03:02.920> know,<00:03:03.280> some<00:03:03.560> priority
[00:03:04] also have um I don't know, some priority
[00:03:04] also have um I don't know, some priority or<00:03:04.240> something.
[00:03:05] or something.
[00:03:05] or something. I<00:03:05.880> do<00:03:06.040> some<00:03:06.240> model<00:03:06.600> change<00:03:06.880> here,<00:03:07.040> so<00:03:07.120> I'm
[00:03:07] I do some model change here, so I'm
[00:03:07] I do some model change here, so I'm going<00:03:07.320> to<00:03:07.360> make<00:03:07.520> this<00:03:07.680> a<00:03:07.760> column.<00:03:09.000> Let's<00:03:09.200> say
[00:03:09] going to make this a column. Let's say
[00:03:09] going to make this a column. Let's say this<00:03:09.520> is<00:03:09.640> an<00:03:09.760> integer.<00:03:10.840> Let's<00:03:11.040> say<00:03:11.160> it's
[00:03:11] this is an integer. Let's say it's
[00:03:11] this is an integer. Let's say it's nullable.
[00:03:13] nullable.
[00:03:13] nullable. Whatever.<00:03:13.960> And<00:03:14.160> I<00:03:14.240> have<00:03:14.440> some<00:03:14.560> changes<00:03:14.960> here.
[00:03:15] Whatever. And I have some changes here.
[00:03:15] Whatever. And I have some changes here. So,<00:03:15.960> I<00:03:16.000> can<00:03:16.120> now<00:03:16.360> exit<00:03:16.920> this<00:03:17.240> editor.<00:03:17.640> I<00:03:17.680> think
[00:03:17] So, I can now exit this editor. I think
[00:03:17] So, I can now exit this editor. I think I<00:03:17.920> need<00:03:18.080> to<00:03:18.160> go<00:03:18.360> to<00:03:18.560> routes.py<00:03:19.360> and<00:03:19.560> save<00:03:19.840> this
[00:03:19] I need to go to routes.py and save this
[00:03:20] I need to go to routes.py and save this as<00:03:20.200> well.
[00:03:21] as well.
[00:03:21] as well. But<00:03:22.000> essentially<00:03:22.520> here,<00:03:23.240> now<00:03:23.480> I<00:03:23.520> have<00:03:23.760> some
[00:03:23] But essentially here, now I have some
[00:03:23] But essentially here, now I have some changes.<00:03:24.280> So,<00:03:24.360> if<00:03:24.480> I<00:03:24.560> do<00:03:24.720> get<00:03:24.920> status,<00:03:25.360> you<00:03:25.440> can
[00:03:25] changes. So, if I do get status, you can
[00:03:25] changes. So, if I do get status, you can see<00:03:26.280> there<00:03:26.440> is<00:03:26.640> stuff<00:03:27.160> that<00:03:27.440> is<00:03:27.560> modified<00:03:28.520> even
[00:03:28] see there is stuff that is modified even
[00:03:28] see there is stuff that is modified even if<00:03:28.840> we<00:03:28.920> ignore<00:03:29.200> the<00:03:29.320> caching<00:03:29.760> files.<00:03:30.000> So,<00:03:30.120> we
[00:03:30] if we ignore the caching files. So, we
[00:03:30] if we ignore the caching files. So, we have<00:03:30.400> models.py,<00:03:31.080> routes.py.<00:03:32.200> Now,<00:03:32.640> someone
[00:03:32] have models.py, routes.py. Now, someone
[00:03:33] have models.py, routes.py. Now, someone says<00:03:33.360> there's<00:03:33.600> a<00:03:33.640> bug<00:03:33.880> fix<00:03:34.160> that<00:03:34.360> needs<00:03:34.560> to<00:03:34.680> be
[00:03:34] says there's a bug fix that needs to be
[00:03:34] says there's a bug fix that needs to be done.<00:03:35.000> How<00:03:35.120> do<00:03:35.280> I<00:03:35.320> do<00:03:35.520> this?<00:03:36.200> The<00:03:36.320> easiest<00:03:36.680> way
[00:03:36] done. How do I do this? The easiest way
[00:03:36] done. How do I do this? The easiest way is<00:03:37.200> by<00:03:37.360> using<00:03:37.720> work<00:03:37.960> tree.<00:03:38.320> This<00:03:38.440> is<00:03:38.560> also,<00:03:38.840> in
[00:03:38] is by using work tree. This is also, in
[00:03:38] is by using work tree. This is also, in my<00:03:39.040> opinion,<00:03:39.360> the<00:03:39.440> most<00:03:39.640> professional<00:03:40.160> way.
[00:03:40] my opinion, the most professional way.
[00:03:40] my opinion, the most professional way. So,<00:03:40.680> what<00:03:40.800> we<00:03:40.920> do<00:03:41.040> is<00:03:41.160> we<00:03:41.240> say<00:03:41.440> Git<00:03:42.520> work<00:03:42.920> tree.
[00:03:43] So, what we do is we say Git work tree.
[00:03:43] So, what we do is we say Git work tree. This<00:03:43.760> is,<00:03:43.880> by<00:03:44.000> the<00:03:44.120> way,<00:03:44.240> not<00:03:44.480> a<00:03:44.560> separate<00:03:44.920> tool
[00:03:45] This is, by the way, not a separate tool
[00:03:45] This is, by the way, not a separate tool that<00:03:45.280> you<00:03:45.320> need<00:03:45.480> to<00:03:45.560> install.<00:03:45.880> This<00:03:46.040> is<00:03:46.160> part
[00:03:46] that you need to install. This is part
[00:03:46] that you need to install. This is part of<00:03:46.560> Git.<00:03:46.960> It<00:03:47.120> already<00:03:47.400> exists<00:03:47.720> in<00:03:47.840> Git.<00:03:48.120> This
[00:03:48] of Git. It already exists in Git. This
[00:03:48] of Git. It already exists in Git. This is<00:03:48.400> not<00:03:48.560> something<00:03:48.840> new.<00:03:49.360> This<00:03:49.520> has<00:03:49.680> been
[00:03:49] is not something new. This has been
[00:03:49] is not something new. This has been around<00:03:50.120> for<00:03:50.280> a<00:03:50.360> while.<00:03:51.080> But<00:03:51.200> essentially,<00:03:51.560> I
[00:03:51] around for a while. But essentially, I
[00:03:51] around for a while. But essentially, I can<00:03:51.760> do<00:03:51.880> now<00:03:52.120> Git<00:03:52.440> work<00:03:52.680> tree<00:03:53.120> at,<00:03:53.880> and<00:03:54.000> then<00:03:54.160> I
[00:03:54] can do now Git work tree at, and then I
[00:03:54] can do now Git work tree at, and then I want<00:03:54.400> to<00:03:54.480> use<00:03:55.200> the<00:03:55.480> parent<00:03:55.840> directory<00:03:56.320> here.
[00:03:56] want to use the parent directory here.
[00:03:56] want to use the parent directory here. I'm<00:03:56.880> going<00:03:57.000> to<00:03:57.080> explain<00:03:57.360> in<00:03:57.400> a<00:03:57.440> second<00:03:57.880> why,
[00:03:58] I'm going to explain in a second why,
[00:03:58] I'm going to explain in a second why, but<00:03:58.520> actually,<00:03:58.760> I<00:03:58.800> need<00:03:58.960> to<00:03:59.080> do<00:03:59.280> this<00:03:59.600> one
[00:03:59] but actually, I need to do this one
[00:03:59] but actually, I need to do this one directory<00:04:00.320> up,<00:04:00.480> so<00:04:00.640> I<00:04:00.680> need<00:04:00.920> to<00:04:01.080> not<00:04:01.320> be<00:04:01.440> in<00:04:01.520> the
[00:04:01] directory up, so I need to not be in the
[00:04:01] directory up, so I need to not be in the Flask<00:04:01.960> application,<00:04:02.520> but<00:04:02.640> actually<00:04:03.000> in<00:04:03.160> the
[00:04:03] Flask application, but actually in the
[00:04:03] Flask application, but actually in the Git<00:04:03.480> repository.<00:04:04.120> So,<00:04:04.280> work<00:04:04.560> tree<00:04:04.720> tutorial
[00:04:05] Git repository. So, work tree tutorial
[00:04:05] Git repository. So, work tree tutorial is<00:04:05.400> the<00:04:05.480> Git<00:04:05.680> repository.<00:04:06.640> Here,<00:04:06.840> I<00:04:06.920> need<00:04:07.080> to
[00:04:07] is the Git repository. Here, I need to
[00:04:07] is the Git repository. Here, I need to do<00:04:07.360> this<00:04:07.680> command.<00:04:08.080> So,<00:04:08.280> Git<00:04:08.560> work<00:04:08.800> tree
[00:04:10] do this command. So, Git work tree
[00:04:10] do this command. So, Git work tree at,<00:04:10.520> and<00:04:10.680> then<00:04:11.480> I<00:04:11.560> want<00:04:11.800> to<00:04:11.920> do<00:04:12.320> the<00:04:12.480> parent
[00:04:12] at, and then I want to do the parent
[00:04:12] at, and then I want to do the parent directory<00:04:13.320> of<00:04:13.520> the<00:04:13.600> repository,<00:04:14.320> so<00:04:14.480> to<00:04:14.600> say,
[00:04:15] directory of the repository, so to say,
[00:04:15] directory of the repository, so to say, and<00:04:15.440> now<00:04:15.560> we<00:04:15.640> can<00:04:15.760> call<00:04:15.960> this<00:04:16.120> something<00:04:16.400> like
[00:04:16] and now we can call this something like
[00:04:16] and now we can call this something like bug<00:04:16.840> fix<00:04:17.160> if<00:04:17.320> that<00:04:17.560> is<00:04:17.680> what<00:04:17.799> the<00:04:17.880> branch
[00:04:18] bug fix if that is what the branch
[00:04:18] bug fix if that is what the branch should<00:04:18.400> be<00:04:18.519> called.<00:04:19.280> So,<00:04:19.359> now<00:04:19.440> I'm<00:04:19.560> creating<00:04:19.959> a
[00:04:20] should be called. So, now I'm creating a
[00:04:20] should be called. So, now I'm creating a work<00:04:20.640> tree<00:04:20.880> called<00:04:21.480> bug<00:04:21.720> fix.<00:04:22.520> Now,<00:04:22.920> you<00:04:23.040> can
[00:04:23] work tree called bug fix. Now, you can
[00:04:23] work tree called bug fix. Now, you can see<00:04:23.320> that<00:04:23.480> in<00:04:23.560> this<00:04:23.720> directory,<00:04:24.200> nothing
[00:04:24] see that in this directory, nothing
[00:04:24] see that in this directory, nothing changes.<00:04:24.880> I<00:04:24.960> still<00:04:25.200> have<00:04:25.360> my<00:04:25.480> Flask
[00:04:25] changes. I still have my Flask
[00:04:25] changes. I still have my Flask application.<00:04:26.440> If<00:04:26.560> I<00:04:26.640> do<00:04:26.840> get<00:04:27.040> status,<00:04:27.480> I<00:04:27.600> still
[00:04:27] application. If I do get status, I still
[00:04:27] application. If I do get status, I still have<00:04:28.080> the<00:04:28.160> files<00:04:28.520> here,<00:04:28.720> models<00:04:29.160> and<00:04:29.320> routes,
[00:04:29] have the files here, models and routes,
[00:04:30] have the files here, models and routes, which<00:04:30.320> are<00:04:30.480> adjusted<00:04:31.000> with<00:04:31.240> the<00:04:31.480> current
[00:04:31] which are adjusted with the current
[00:04:31] which are adjusted with the current changes.<00:04:32.240> Maybe<00:04:32.440> it's<00:04:32.560> not<00:04:32.720> complete<00:04:33.080> yet,
[00:04:33] changes. Maybe it's not complete yet,
[00:04:33] changes. Maybe it's not complete yet, but<00:04:33.400> my<00:04:33.520> code<00:04:33.760> is<00:04:33.840> still<00:04:34.080> there.<00:04:34.520> But<00:04:34.640> at<00:04:34.720> the
[00:04:34] but my code is still there. But at the
[00:04:34] but my code is still there. But at the same<00:04:35.080> time,<00:04:35.360> now<00:04:35.520> I<00:04:35.600> can<00:04:35.760> go<00:04:36.000> up<00:04:36.280> one
[00:04:36] same time, now I can go up one
[00:04:36] same time, now I can go up one directory,<00:04:37.080> and<00:04:37.240> now<00:04:37.400> I<00:04:37.440> can<00:04:37.600> see<00:04:37.760> on<00:04:37.880> the<00:04:37.960> same
[00:04:38] directory, and now I can see on the same
[00:04:38] directory, and now I can see on the same level<00:04:38.600> as<00:04:38.840> work<00:04:39.080> tree<00:04:39.240> tutorial,<00:04:40.160> I<00:04:40.320> have<00:04:40.600> a
[00:04:40] level as work tree tutorial, I have a
[00:04:40] level as work tree tutorial, I have a directory<00:04:41.200> called<00:04:41.600> bug<00:04:41.840> fix.<00:04:42.360> And<00:04:42.720> now<00:04:42.920> I<00:04:43.000> can
[00:04:43] directory called bug fix. And now I can
[00:04:43] directory called bug fix. And now I can go<00:04:43.360> into<00:04:43.640> this<00:04:43.800> directory,
[00:04:45] go into this directory,
[00:04:45] go into this directory, and<00:04:45.320> you<00:04:45.400> can<00:04:45.520> see<00:04:45.720> I<00:04:45.800> also<00:04:46.080> have<00:04:46.320> here<00:04:46.520> the
[00:04:46] and you can see I also have here the
[00:04:46] and you can see I also have here the Flask<00:04:46.960> to<00:04:47.040> do<00:04:47.200> application.<00:04:47.920> I<00:04:48.040> even<00:04:48.320> have<00:04:48.520> the
[00:04:48] Flask to do application. I even have the
[00:04:48] Flask to do application. I even have the readme<00:04:49.000> file<00:04:49.280> here<00:04:49.480> still.<00:04:50.200> And<00:04:50.320> if<00:04:50.440> I<00:04:50.480> do<00:04:50.720> get
[00:04:50] readme file here still. And if I do get
[00:04:50] readme file here still. And if I do get status<00:04:51.440> here,<00:04:51.960> you<00:04:52.120> can<00:04:52.240> see<00:04:52.560> that<00:04:52.680> there's
[00:04:52] status here, you can see that there's
[00:04:52] status here, you can see that there's nothing<00:04:53.280> to<00:04:53.400> commit.<00:04:53.880> This<00:04:54.120> is<00:04:54.400> basically
[00:04:55] nothing to commit. This is basically
[00:04:55] nothing to commit. This is basically what<00:04:55.520> is<00:04:55.720> in<00:04:55.920> the<00:04:56.000> repository<00:04:56.960> without<00:04:57.320> any
[00:04:57] what is in the repository without any
[00:04:57] what is in the repository without any changes.<00:04:58.160> And<00:04:58.280> then,<00:04:58.400> of<00:04:58.520> course,<00:04:58.760> what<00:04:58.880> I<00:04:58.920> can
[00:04:59] changes. And then, of course, what I can
[00:04:59] changes. And then, of course, what I can do<00:04:59.240> here<00:04:59.560> is<00:04:59.800> I<00:04:59.920> can<00:05:00.080> go<00:05:00.320> into<00:05:00.560> the<00:05:00.640> project.<00:05:01.400> I
[00:05:01] do here is I can go into the project. I
[00:05:01] do here is I can go into the project. I can<00:05:01.640> go<00:05:01.800> into<00:05:02.040> Flask<00:05:02.320> to<00:05:02.440> do<00:05:02.640> app,<00:05:02.960> and<00:05:03.200> I<00:05:03.280> can
[00:05:03] can go into Flask to do app, and I can
[00:05:03] can go into Flask to do app, and I can do<00:05:04.000> the<00:05:04.120> bug<00:05:04.320> fix,<00:05:04.720> whatever<00:05:05.040> that<00:05:05.320> bug<00:05:05.560> fix
[00:05:05] do the bug fix, whatever that bug fix
[00:05:05] do the bug fix, whatever that bug fix is.<00:05:06.200> Maybe<00:05:06.520> it's<00:05:06.680> just<00:05:06.920> going<00:05:07.120> to<00:05:07.200> the
[00:05:07] is. Maybe it's just going to the
[00:05:07] is. Maybe it's just going to the template<00:05:08.240> and<00:05:08.600> renaming<00:05:09.080> the<00:05:09.200> application.
[00:05:09] template and renaming the application.
[00:05:09] template and renaming the application. It<00:05:09.920> shouldn't<00:05:10.160> be<00:05:10.240> called<00:05:10.520> to<00:05:10.600> do<00:05:10.800> app.<00:05:11.040> It
[00:05:11] It shouldn't be called to do app. It
[00:05:11] It shouldn't be called to do app. It should<00:05:11.360> be<00:05:11.480> called<00:05:12.240> to<00:05:12.400> do<00:05:12.560> application.<00:05:13.400> That
[00:05:13] should be called to do application. That
[00:05:13] should be called to do application. That is<00:05:13.840> the<00:05:13.920> quick<00:05:14.160> fix<00:05:14.440> I<00:05:14.480> need<00:05:14.680> to<00:05:14.800> make,<00:05:15.080> and<00:05:15.200> I
[00:05:15] is the quick fix I need to make, and I
[00:05:15] is the quick fix I need to make, and I also<00:05:15.480> need<00:05:15.640> to<00:05:15.720> do<00:05:15.880> it<00:05:16.000> here.<00:05:16.920> To<00:05:17.120> do
[00:05:17] also need to do it here. To do
[00:05:17] also need to do it here. To do application.
[00:05:20] application.
[00:05:20] application. That<00:05:20.520> is<00:05:20.640> basically<00:05:21.120> it.
[00:05:22] That is basically it.
[00:05:22] That is basically it. And<00:05:22.760> I<00:05:22.960> still<00:05:23.320> keep<00:05:23.600> the<00:05:23.720> changes<00:05:24.240> that<00:05:24.400> I
[00:05:24] And I still keep the changes that I
[00:05:24] And I still keep the changes that I started<00:05:24.960> making<00:05:25.400> in<00:05:25.840> the<00:05:25.920> main<00:05:26.160> branch<00:05:26.520> in<00:05:26.640> my
[00:05:26] started making in the main branch in my
[00:05:26] started making in the main branch in my repository<00:05:27.480> here,<00:05:28.280> but<00:05:28.480> I<00:05:28.560> can<00:05:28.680> now<00:05:28.840> go<00:05:29.000> ahead
[00:05:29] repository here, but I can now go ahead
[00:05:29] repository here, but I can now go ahead and<00:05:29.400> say<00:05:29.640> Git<00:05:30.080> at<00:05:30.800> and<00:05:31.320> the<00:05:31.440> Flask<00:05:31.760> to<00:05:31.840> do<00:05:32.040> app.
[00:05:32] and say Git at and the Flask to do app.
[00:05:32] and say Git at and the Flask to do app. I<00:05:32.280> can<00:05:32.440> say<00:05:32.600> Git<00:05:32.800> status.<00:05:33.200> This<00:05:33.480> adds<00:05:33.840> the<00:05:33.960> base
[00:05:34] I can say Git status. This adds the base
[00:05:34] I can say Git status. This adds the base HTML<00:05:34.720> file<00:05:34.960> here,<00:05:35.600> and<00:05:35.680> now<00:05:35.760> I<00:05:35.800> can<00:05:35.960> say<00:05:36.160> Git
[00:05:36] HTML file here, and now I can say Git
[00:05:36] HTML file here, and now I can say Git commit,<00:05:37.600> and<00:05:37.760> I<00:05:37.840> can<00:05:38.000> say,<00:05:38.360> I<00:05:38.400> don't<00:05:38.560> know,
[00:05:38] commit, and I can say, I don't know,
[00:05:38] commit, and I can say, I don't know, title<00:05:39.760> fix<00:05:40.600> or<00:05:40.720> something<00:05:41.000> like<00:05:41.160> this.<00:05:41.560> I<00:05:41.640> can
[00:05:41] title fix or something like this. I can
[00:05:41] title fix or something like this. I can push<00:05:42.160> this.<00:05:43.040> And<00:05:43.760> the<00:05:43.880> only<00:05:44.080> thing,<00:05:44.240> of
[00:05:44] push this. And the only thing, of
[00:05:44] push this. And the only thing, of course,<00:05:44.520> is<00:05:44.640> I<00:05:44.720> need<00:05:44.920> to<00:05:45.200> set<00:05:45.760> the<00:05:45.960> upstream
[00:05:46] course, is I need to set the upstream
[00:05:46] course, is I need to set the upstream branch<00:05:46.840> to<00:05:46.960> be<00:05:47.080> the<00:05:47.200> proper<00:05:47.560> one,<00:05:47.960> origin<00:05:48.880> bug
[00:05:49] branch to be the proper one, origin bug
[00:05:49] branch to be the proper one, origin bug fix.
[00:05:50] fix.
[00:05:50] fix. And<00:05:50.920> this<00:05:51.160> now<00:05:51.400> is<00:05:51.560> being<00:05:51.800> pushed<00:05:52.360> to<00:05:52.480> my<00:05:52.640> Git
[00:05:53] And this now is being pushed to my Git
[00:05:53] And this now is being pushed to my Git repository.<00:05:53.960> So,<00:05:54.080> if<00:05:54.200> I<00:05:54.280> refresh<00:05:54.760> here,<00:05:55.600> you
[00:05:55] repository. So, if I refresh here, you
[00:05:55] repository. So, if I refresh here, you will<00:05:55.920> see<00:05:56.360> that<00:05:56.560> on<00:05:56.720> work<00:05:56.960> tree<00:05:57.120> tutorial,<00:05:57.840> I
[00:05:57] will see that on work tree tutorial, I
[00:05:58] will see that on work tree tutorial, I have<00:05:58.560> a<00:05:58.840> branch<00:05:59.480> called<00:05:59.840> bug<00:06:00.080> fix,<00:06:00.640> and<00:06:01.040> in
[00:06:01] have a branch called bug fix, and in
[00:06:01] have a branch called bug fix, and in this<00:06:01.480> bug<00:06:01.680> fix<00:06:01.960> branch<00:06:02.320> here,<00:06:02.960> I<00:06:03.080> changed<00:06:03.440> the
[00:06:03] this bug fix branch here, I changed the
[00:06:03] this bug fix branch here, I changed the title.<00:06:04.240> However,<00:06:04.760> I<00:06:04.840> can<00:06:05.000> still<00:06:05.240> go<00:06:05.400> back<00:06:05.680> now
[00:06:06] title. However, I can still go back now
[00:06:06] title. However, I can still go back now and<00:06:07.120> continue<00:06:07.520> to<00:06:07.680> work<00:06:08.360> on<00:06:08.600> work<00:06:08.840> tree
[00:06:09] and continue to work on work tree
[00:06:09] and continue to work on work tree tutorial,<00:06:10.360> which<00:06:11.040> will<00:06:11.200> still<00:06:11.520> have,<00:06:11.800> if<00:06:11.920> I<00:06:12.000> do
[00:06:12] tutorial, which will still have, if I do
[00:06:12] tutorial, which will still have, if I do get<00:06:12.360> status,<00:06:13.360> models<00:06:13.880> and<00:06:14.040> routes<00:06:14.480> changed
[00:06:14] get status, models and routes changed
[00:06:14] get status, models and routes changed with<00:06:15.160> the<00:06:15.240> new<00:06:15.440> field,<00:06:15.800> which<00:06:15.920> was<00:06:16.080> priority,
[00:06:16] with the new field, which was priority,
[00:06:16] with the new field, which was priority, and<00:06:16.960> also<00:06:17.240> with<00:06:17.480> whatever<00:06:17.800> we<00:06:17.920> did<00:06:18.240> in
[00:06:18] and also with whatever we did in
[00:06:18] and also with whatever we did in routes.py.<00:06:19.800> That<00:06:20.120> is<00:06:20.280> the<00:06:20.360> basic<00:06:20.640> idea<00:06:21.000> of<00:06:21.160> Git
[00:06:21] routes.py. That is the basic idea of Git
[00:06:21] routes.py. That is the basic idea of Git work<00:06:21.600> trees,<00:06:21.800> and<00:06:21.920> we<00:06:22.040> can<00:06:22.240> do<00:06:22.440> that<00:06:23.080> not<00:06:23.360> just
[00:06:23] work trees, and we can do that not just
[00:06:23] work trees, and we can do that not just once,<00:06:23.920> we<00:06:24.040> can<00:06:24.200> do<00:06:24.320> that<00:06:24.560> five<00:06:24.840> times.<00:06:25.240> I<00:06:25.320> can
[00:06:25] once, we can do that five times. I can
[00:06:25] once, we can do that five times. I can go<00:06:26.040> here<00:06:26.760> and<00:06:26.960> say<00:06:27.200> Git<00:06:27.920> work<00:06:28.200> tree
[00:06:29] go here and say Git work tree
[00:06:29] go here and say Git work tree at,<00:06:29.680> and<00:06:29.800> I<00:06:29.880> can<00:06:30.040> make<00:06:30.240> another<00:06:30.480> one,<00:06:30.720> like<00:06:31.040> UI
[00:06:31] at, and I can make another one, like UI
[00:06:31] at, and I can make another one, like UI changes,<00:06:32.160> for<00:06:32.320> example.<00:06:32.760> And<00:06:32.880> I<00:06:32.920> can<00:06:33.120> do<00:06:33.320> one
[00:06:33] changes, for example. And I can do one
[00:06:33] changes, for example. And I can do one that<00:06:33.960> is<00:06:34.120> maybe<00:06:35.000> performance<00:06:35.960> or<00:06:36.080> something
[00:06:36] that is maybe performance or something
[00:06:36] that is maybe performance or something like<00:06:36.520> this.<00:06:37.040> And<00:06:37.200> all<00:06:37.360> of<00:06:37.480> these<00:06:37.720> are<00:06:37.840> now<00:06:38.000> at
[00:06:38] like this. And all of these are now at
[00:06:38] like this. And all of these are now at the<00:06:38.160> same<00:06:38.400> level,<00:06:38.760> which<00:06:38.920> is<00:06:39.040> also<00:06:39.320> why<00:06:39.520> we
[00:06:39] the same level, which is also why we
[00:06:39] the same level, which is also why we used<00:06:39.920> a<00:06:39.960> parent<00:06:40.200> directory.<00:06:41.000> Of<00:06:41.120> course,<00:06:41.360> I
[00:06:41] used a parent directory. Of course, I
[00:06:41] used a parent directory. Of course, I can<00:06:41.520> also<00:06:41.720> just<00:06:41.880> say<00:06:42.040> Git<00:06:42.320> work<00:06:42.560> tree<00:06:43.440> if<00:06:43.640> I'm
[00:06:43] can also just say Git work tree if I'm
[00:06:43] can also just say Git work tree if I'm now<00:06:44.040> in<00:06:44.440> the<00:06:44.520> repository.
[00:06:46] now in the repository.
[00:06:46] now in the repository. I<00:06:46.200> can<00:06:46.360> say<00:06:46.520> Git<00:06:46.760> work<00:06:47.000> tree<00:06:47.880> at,<00:06:48.160> and<00:06:48.280> then
[00:06:49] I can say Git work tree at, and then
[00:06:49] I can say Git work tree at, and then something<00:06:49.480> in<00:06:49.640> here.<00:06:49.920> This<00:06:50.080> would<00:06:50.200> also<00:06:50.480> work,
[00:06:51] something in here. This would also work,
[00:06:51] something in here. This would also work, but<00:06:51.280> now<00:06:51.440> I<00:06:51.480> would<00:06:51.640> have<00:06:51.880> a<00:06:51.920> Git<00:06:52.160> repository<00:06:52.840> in
[00:06:52] but now I would have a Git repository in
[00:06:52] but now I would have a Git repository in a<00:06:53.000> Git<00:06:53.160> repository.<00:06:53.840> That's<00:06:54.120> just<00:06:54.320> not<00:06:54.600> nice.
[00:06:54] a Git repository. That's just not nice.
[00:06:54] a Git repository. That's just not nice. That's<00:06:55.160> not<00:06:55.760> easy<00:06:56.000> to<00:06:56.160> work<00:06:56.400> with.<00:06:57.040> So,<00:06:57.200> I<00:06:57.280> can
[00:06:57] That's not easy to work with. So, I can
[00:06:57] That's not easy to work with. So, I can also<00:06:57.600> remove<00:06:57.880> this<00:06:58.040> now,<00:06:58.400> Git<00:06:58.760> work<00:06:59.040> tree
[00:06:59] also remove this now, Git work tree
[00:06:59] also remove this now, Git work tree remove<00:07:00.800> something.
[00:07:02] remove something.
[00:07:02] remove something. And<00:07:03.040> this<00:07:03.240> also<00:07:03.520> deletes<00:07:03.880> the<00:07:03.960> directory,<00:07:04.960> but
[00:07:05] And this also deletes the directory, but
[00:07:05] And this also deletes the directory, but now<00:07:05.320> here<00:07:05.520> I<00:07:05.600> have<00:07:05.840> basically<00:07:06.320> four<00:07:06.600> times<00:07:06.960> the
[00:07:07] now here I have basically four times the
[00:07:07] now here I have basically four times the same<00:07:07.240> repository,<00:07:08.040> and<00:07:08.160> I<00:07:08.240> can<00:07:08.440> do<00:07:08.680> some<00:07:08.880> bug
[00:07:09] same repository, and I can do some bug
[00:07:09] same repository, and I can do some bug fixes<00:07:09.400> in<00:07:09.440> the<00:07:09.520> first<00:07:09.840> one,<00:07:10.040> performance
[00:07:10] fixes in the first one, performance
[00:07:10] fixes in the first one, performance optimizations<00:07:11.160> in<00:07:11.240> the<00:07:11.320> second<00:07:11.640> one,<00:07:11.880> UI
[00:07:12] optimizations in the second one, UI
[00:07:12] optimizations in the second one, UI changes<00:07:12.560> in<00:07:12.640> the<00:07:12.760> third<00:07:13.040> one,<00:07:13.520> and<00:07:13.600> then
[00:07:13] changes in the third one, and then
[00:07:13] changes in the third one, and then whatever<00:07:14.040> I<00:07:14.120> started<00:07:14.520> doing<00:07:14.800> in<00:07:14.920> the<00:07:15.000> main
[00:07:15] whatever I started doing in the main
[00:07:15] whatever I started doing in the main one,<00:07:15.800> and<00:07:15.920> that's<00:07:16.160> basically<00:07:16.560> it.<00:07:16.600> Then<00:07:16.800> I<00:07:16.840> can
[00:07:16] one, and that's basically it. Then I can
[00:07:17] one, and that's basically it. Then I can push<00:07:17.200> all<00:07:17.360> of<00:07:17.480> them.<00:07:17.680> I<00:07:17.760> can<00:07:17.880> also<00:07:18.080> merge<00:07:18.360> all
[00:07:18] push all of them. I can also merge all
[00:07:18] push all of them. I can also merge all of<00:07:18.600> them<00:07:18.720> again.<00:07:19.480> That<00:07:19.840> is<00:07:20.000> just<00:07:20.240> having
[00:07:20] of them again. That is just having
[00:07:20] of them again. That is just having multiple<00:07:21.040> branches<00:07:21.640> checked<00:07:22.000> out<00:07:22.240> at<00:07:22.320> the
[00:07:22] multiple branches checked out at the
[00:07:22] multiple branches checked out at the same<00:07:22.720> time<00:07:23.400> locally.<00:07:24.120> Okay,<00:07:24.360> so<00:07:24.560> how<00:07:24.720> does<00:07:24.960> all
[00:07:25] same time locally. Okay, so how does all
[00:07:25] same time locally. Okay, so how does all of<00:07:25.240> this<00:07:25.440> now<00:07:25.680> relate<00:07:26.200> to<00:07:26.440> agentic<00:07:26.960> coding?
[00:07:27] of this now relate to agentic coding?
[00:07:27] of this now relate to agentic coding? Now,<00:07:27.760> I<00:07:27.840> deleted<00:07:28.280> all<00:07:28.480> these<00:07:28.960> other<00:07:29.640> Git<00:07:29.920> work
[00:07:30] Now, I deleted all these other Git work
[00:07:30] Now, I deleted all these other Git work trees<00:07:30.400> here,<00:07:30.760> and<00:07:31.240> I'm<00:07:31.360> going<00:07:31.520> to<00:07:31.640> show<00:07:31.760> you
[00:07:31] trees here, and I'm going to show you
[00:07:31] trees here, and I'm going to show you how<00:07:31.960> we<00:07:32.080> can<00:07:32.200> do<00:07:32.320> the<00:07:32.440> same<00:07:32.720> thing<00:07:33.520> with
[00:07:33] how we can do the same thing with
[00:07:34] how we can do the same thing with something<00:07:34.320> like<00:07:34.520> Cloud<00:07:34.840> Code.<00:07:35.200> Now,<00:07:35.400> here's<00:07:35.680> a
[00:07:35] something like Cloud Code. Now, here's a
[00:07:35] something like Cloud Code. Now, here's a side<00:07:36.000> note.<00:07:36.240> If<00:07:36.360> you<00:07:36.440> want<00:07:36.640> to<00:07:36.720> do<00:07:36.920> this<00:07:37.120> with
[00:07:37] side note. If you want to do this with
[00:07:37] side note. If you want to do this with something<00:07:37.720> like<00:07:38.040> Open<00:07:38.320> Code,<00:07:38.800> there<00:07:39.040> is<00:07:39.280> not
[00:07:39] something like Open Code, there is not
[00:07:39] something like Open Code, there is not an<00:07:39.760> automated<00:07:40.280> feature,<00:07:40.680> at<00:07:40.840> least<00:07:41.080> as<00:07:41.240> of
[00:07:41] an automated feature, at least as of
[00:07:41] an automated feature, at least as of right<00:07:41.600> now.<00:07:41.920> In<00:07:42.160> Open<00:07:42.440> Code,<00:07:42.760> what<00:07:42.920> you<00:07:43.000> would
[00:07:43] right now. In Open Code, what you would
[00:07:43] right now. In Open Code, what you would have<00:07:43.400> to<00:07:43.480> do<00:07:43.600> is<00:07:43.720> you<00:07:43.760> would<00:07:43.880> have<00:07:44.120> to<00:07:44.200> create
[00:07:44] have to do is you would have to create
[00:07:44] have to do is you would have to create these<00:07:44.720> work<00:07:44.920> trees<00:07:45.160> yourself<00:07:45.600> the<00:07:45.680> same<00:07:45.880> way<00:07:46.080> I
[00:07:46] these work trees yourself the same way I
[00:07:46] these work trees yourself the same way I did<00:07:46.320> it<00:07:46.440> just<00:07:46.640> now,<00:07:47.200> and<00:07:47.320> you<00:07:47.400> start<00:07:47.720> multiple
[00:07:48] did it just now, and you start multiple
[00:07:48] did it just now, and you start multiple Open<00:07:48.520> Code<00:07:48.840> instances.<00:07:49.400> So,<00:07:49.520> you<00:07:49.600> start<00:07:50.280> one
[00:07:50] Open Code instances. So, you start one
[00:07:50] Open Code instances. So, you start one in<00:07:50.600> folder<00:07:50.920> one,<00:07:51.400> one<00:07:51.560> in<00:07:51.640> folder<00:07:51.920> two,<00:07:52.080> and<00:07:52.240> so
[00:07:52] in folder one, one in folder two, and so
[00:07:52] in folder one, one in folder two, and so on,<00:07:52.560> and<00:07:52.640> then<00:07:52.760> you<00:07:52.840> just<00:07:53.040> have<00:07:53.280> four<00:07:53.560> Open
[00:07:53] on, and then you just have four Open
[00:07:53] on, and then you just have four Open Code<00:07:54.120> instances.<00:07:55.160> That<00:07:55.520> is<00:07:55.840> the<00:07:55.960> main<00:07:56.160> use
[00:07:56] Code instances. That is the main use
[00:07:56] Code instances. That is the main use case<00:07:56.560> here.<00:07:57.000> What<00:07:57.160> you<00:07:57.240> want<00:07:57.360> to<00:07:57.440> do<00:07:57.600> is<00:07:57.720> you
[00:07:57] case here. What you want to do is you
[00:07:57] case here. What you want to do is you want<00:07:57.920> to<00:07:58.000> have<00:07:58.200> four<00:07:58.440> coding<00:07:58.840> agents,<00:07:59.200> maybe
[00:07:59] want to have four coding agents, maybe
[00:07:59] want to have four coding agents, maybe on<00:07:59.520> four<00:07:59.680> different<00:08:00.120> screens,<00:08:00.400> maybe<00:08:00.600> you
[00:08:00] on four different screens, maybe you
[00:08:00] on four different screens, maybe you want<00:08:00.760> to<00:08:00.840> do<00:08:00.920> split<00:08:01.160> screen<00:08:01.440> or<00:08:01.520> something,
[00:08:02] want to do split screen or something,
[00:08:02] want to do split screen or something, and<00:08:02.600> you<00:08:02.680> want<00:08:02.920> them<00:08:03.240> working<00:08:03.560> on<00:08:03.680> different
[00:08:04] and you want them working on different
[00:08:04] and you want them working on different parts<00:08:04.280> of<00:08:04.360> the<00:08:04.480> application,<00:08:05.200> each<00:08:05.480> pushing
[00:08:06] parts of the application, each pushing
[00:08:06] parts of the application, each pushing onto<00:08:06.480> their<00:08:06.640> own<00:08:06.800> branches<00:08:07.320> as<00:08:07.480> if<00:08:07.640> they<00:08:07.760> were
[00:08:08] onto their own branches as if they were
[00:08:08] onto their own branches as if they were employees,<00:08:08.680> and<00:08:08.800> then<00:08:08.960> merging<00:08:09.280> them
[00:08:09] employees, and then merging them
[00:08:09] employees, and then merging them together.<00:08:10.120> This<00:08:10.280> is<00:08:10.400> what<00:08:10.560> you<00:08:10.680> do<00:08:10.840> then
[00:08:11] together. This is what you do then
[00:08:11] together. This is what you do then essentially.
[00:08:12] essentially.
[00:08:12] essentially. But<00:08:12.520> with<00:08:12.680> Cloud<00:08:13.000> Code,<00:08:13.200> you<00:08:13.280> don't<00:08:13.440> even<00:08:13.600> need
[00:08:13] But with Cloud Code, you don't even need
[00:08:13] But with Cloud Code, you don't even need to<00:08:13.840> set<00:08:13.960> up<00:08:14.040> the<00:08:14.160> work<00:08:14.360> trees.<00:08:14.600> You<00:08:14.680> can<00:08:14.840> just
[00:08:15] to set up the work trees. You can just
[00:08:15] to set up the work trees. You can just go<00:08:15.640> into<00:08:16.080> your
[00:08:17] go into your
[00:08:17] go into your Git<00:08:17.280> repository<00:08:17.920> here,<00:08:18.160> work<00:08:18.360> tree<00:08:18.480> tutorial,
[00:08:19] Git repository here, work tree tutorial,
[00:08:19] Git repository here, work tree tutorial, and<00:08:19.680> you<00:08:19.760> can<00:08:19.880> say<00:08:20.200> Cloud<00:08:21.480> --work<00:08:22.760> tree.<00:08:23.320> This
[00:08:23] and you can say Cloud --work tree. This
[00:08:23] and you can say Cloud --work tree. This is<00:08:23.760> enough<00:08:24.560> to<00:08:24.680> create<00:08:25.040> a<00:08:25.120> Cloud<00:08:25.520> instance
[00:08:26] is enough to create a Cloud instance
[00:08:26] is enough to create a Cloud instance that<00:08:26.480> creates<00:08:26.800> a<00:08:26.880> work<00:08:27.160> tree.<00:08:27.480> And<00:08:27.600> you<00:08:27.680> can
[00:08:27] that creates a work tree. And you can
[00:08:27] that creates a work tree. And you can see<00:08:28.000> here<00:08:28.200> this<00:08:28.360> is<00:08:28.480> created<00:08:28.800> in<00:08:28.920> the<00:08:29.000> dot
[00:08:29] see here this is created in the dot
[00:08:29] see here this is created in the dot Cloud<00:08:29.680> directory<00:08:30.640> of<00:08:30.760> this<00:08:30.920> project,<00:08:31.480> work
[00:08:31] Cloud directory of this project, work
[00:08:31] Cloud directory of this project, work trees,<00:08:32.000> and<00:08:32.159> it's<00:08:32.400> given<00:08:32.680> a<00:08:32.719> default<00:08:33.240> name,
[00:08:33] trees, and it's given a default name,
[00:08:33] trees, and it's given a default name, cryptic<00:08:34.200> toasting<00:08:34.599> storm.<00:08:35.400> Then<00:08:35.560> I<00:08:35.640> can<00:08:35.760> do
[00:08:35] cryptic toasting storm. Then I can do
[00:08:35] cryptic toasting storm. Then I can do something<00:08:36.240> here.<00:08:36.440> I<00:08:36.479> can<00:08:36.640> say,<00:08:36.960> for<00:08:37.120> example,
[00:08:38] something here. I can say, for example,
[00:08:38] something here. I can say, for example, make<00:08:39.400> the<00:08:39.680> style<00:08:40.440> of<00:08:40.800> the<00:08:41.080> entire<00:08:42.000> application
[00:08:43] make the style of the entire application
[00:08:43] make the style of the entire application red.
[00:08:44] red.
[00:08:44] red. That<00:08:45.040> would<00:08:45.160> be<00:08:45.320> one<00:08:45.560> instance.<00:08:46.240> Now,<00:08:46.360> at<00:08:46.440> the
[00:08:46] That would be one instance. Now, at the
[00:08:46] That would be one instance. Now, at the same<00:08:46.800> time,<00:08:47.040> I<00:08:47.080> can<00:08:47.240> open<00:08:47.440> up<00:08:47.680> a<00:08:47.800> second
[00:08:48] same time, I can open up a second
[00:08:48] same time, I can open up a second terminal<00:08:48.560> window,
[00:08:50] terminal window,
[00:08:50] terminal window, and<00:08:50.360> I<00:08:50.400> can<00:08:50.560> go<00:08:50.720> to<00:08:50.840> the<00:08:50.920> same<00:08:51.120> directory,<00:08:51.640> so
[00:08:51] and I can go to the same directory, so
[00:08:51] and I can go to the same directory, so that<00:08:51.960> would<00:08:52.120> be<00:08:52.240> tutorial,<00:08:52.840> that<00:08:53.000> would<00:08:53.120> be
[00:08:53] that would be tutorial, that would be
[00:08:53] that would be tutorial, that would be work<00:08:53.520> tree<00:08:53.680> tutorial,<00:08:54.760> and<00:08:54.920> I<00:08:54.960> would<00:08:55.160> say
[00:08:55] work tree tutorial, and I would say
[00:08:55] work tree tutorial, and I would say Cloud<00:08:56.480> --work<00:08:57.240> tree<00:08:57.520> again,<00:08:58.280> and<00:08:58.440> I<00:08:58.480> can<00:08:58.640> also,
[00:08:58] Cloud --work tree again, and I can also,
[00:08:58] Cloud --work tree again, and I can also, if<00:08:59.080> I<00:08:59.160> want<00:08:59.400> to,<00:08:59.480> provide<00:08:59.840> a<00:08:59.880> name.<00:09:00.240> So,<00:09:00.360> I<00:09:00.400> can
[00:09:00] if I want to, provide a name. So, I can
[00:09:00] if I want to, provide a name. So, I can say,<00:09:00.720> for<00:09:00.840> example,<00:09:02.080> uh<00:09:02.400> green<00:09:02.800> theme<00:09:03.400> or
[00:09:03] say, for example, uh green theme or
[00:09:03] say, for example, uh green theme or something<00:09:03.840> like<00:09:04.000> this.<00:09:04.560> Then<00:09:04.680> it's<00:09:04.800> not<00:09:05.040> going
[00:09:05] something like this. Then it's not going
[00:09:05] something like this. Then it's not going to<00:09:05.280> give<00:09:05.440> me<00:09:05.560> a<00:09:05.600> default<00:09:06.000> name.<00:09:06.200> It<00:09:06.320> creates
[00:09:06] to give me a default name. It creates
[00:09:06] to give me a default name. It creates just<00:09:06.880> a<00:09:06.960> green<00:09:07.280> theme<00:09:08.040> work<00:09:08.280> tree,<00:09:08.400> and<00:09:08.520> I<00:09:08.560> can
[00:09:08] just a green theme work tree, and I can
[00:09:08] just a green theme work tree, and I can say<00:09:09.000> make<00:09:09.440> the<00:09:09.600> entire<00:09:10.920> application
[00:09:12] say make the entire application
[00:09:12] say make the entire application green.
[00:09:14] green.
[00:09:14] green. Um<00:09:15.240> so,<00:09:15.400> what<00:09:15.520> it<00:09:15.600> does<00:09:15.800> here<00:09:16.080> on<00:09:16.280> the<00:09:16.400> left
[00:09:16] Um so, what it does here on the left
[00:09:16] Um so, what it does here on the left side<00:09:16.960> is<00:09:17.120> it<00:09:17.240> adjusts<00:09:17.600> everything.<00:09:18.000> I'm<00:09:18.120> just
[00:09:18] side is it adjusts everything. I'm just
[00:09:18] side is it adjusts everything. I'm just going<00:09:18.400> to<00:09:18.480> allow<00:09:18.720> it<00:09:18.800> to<00:09:18.880> do<00:09:19.040> whatever<00:09:19.280> it
[00:09:19] going to allow it to do whatever it
[00:09:19] going to allow it to do whatever it wants.<00:09:20.120> It<00:09:20.240> adjusts<00:09:20.600> everything,<00:09:21.040> so<00:09:21.240> it's
[00:09:21] wants. It adjusts everything, so it's
[00:09:21] wants. It adjusts everything, so it's red,<00:09:21.760> and<00:09:21.880> on<00:09:21.960> the<00:09:22.040> right<00:09:22.240> side,<00:09:22.440> it's<00:09:22.520> going
[00:09:22] red, and on the right side, it's going
[00:09:22] red, and on the right side, it's going to<00:09:22.760> do<00:09:22.880> the<00:09:23.000> same<00:09:23.200> thing<00:09:23.360> with<00:09:23.560> green.<00:09:23.960> They're
[00:09:24] to do the same thing with green. They're
[00:09:24] to do the same thing with green. They're both<00:09:24.400> going<00:09:24.600> to<00:09:24.680> be<00:09:24.880> then<00:09:25.120> on<00:09:25.280> separate
[00:09:25] both going to be then on separate
[00:09:25] both going to be then on separate branches,<00:09:26.080> and<00:09:26.160> I<00:09:26.240> can<00:09:26.360> compare<00:09:26.760> them.<00:09:26.920> I<00:09:26.960> can
[00:09:27] branches, and I can compare them. I can
[00:09:27] branches, and I can compare them. I can play<00:09:27.320> around<00:09:27.600> with<00:09:27.720> them<00:09:27.840> if<00:09:27.960> I<00:09:28.040> want<00:09:28.320> to.<00:09:28.920> That
[00:09:29] play around with them if I want to. That
[00:09:29] play around with them if I want to. That is<00:09:29.240> not<00:09:29.480> necessarily<00:09:30.000> how<00:09:30.120> you<00:09:30.240> want<00:09:30.400> to<00:09:30.480> do
[00:09:30] is not necessarily how you want to do
[00:09:30] is not necessarily how you want to do this.<00:09:30.960> I<00:09:31.000> mean,<00:09:31.200> you<00:09:31.320> can<00:09:31.640> also<00:09:31.920> do<00:09:32.120> this<00:09:32.320> to
[00:09:32] this. I mean, you can also do this to
[00:09:32] this. I mean, you can also do this to try<00:09:32.680> different<00:09:33.000> approaches<00:09:33.440> for<00:09:33.520> the<00:09:33.600> same
[00:09:33] try different approaches for the same
[00:09:33] try different approaches for the same problem,<00:09:34.680> but<00:09:34.800> the<00:09:34.920> ideal<00:09:35.360> use<00:09:35.560> case<00:09:35.960> would
[00:09:36] problem, but the ideal use case would
[00:09:36] problem, but the ideal use case would probably<00:09:37.000> be<00:09:37.200> something<00:09:37.680> like<00:09:37.960> you<00:09:38.080> have<00:09:39.040> one
[00:09:39] probably be something like you have one
[00:09:39] probably be something like you have one Cloud<00:09:39.840> agent<00:09:40.280> working<00:09:40.680> on<00:09:40.840> the<00:09:40.920> style,<00:09:41.560> one
[00:09:41] Cloud agent working on the style, one
[00:09:41] Cloud agent working on the style, one Cloud<00:09:42.120> agent<00:09:42.480> working<00:09:42.760> on<00:09:42.880> performance,<00:09:43.480> one
[00:09:43] Cloud agent working on performance, one
[00:09:43] Cloud agent working on performance, one Cloud<00:09:44.000> agent<00:09:44.320> working<00:09:44.680> on<00:09:44.920> a<00:09:44.960> database
[00:09:45] Cloud agent working on a database
[00:09:45] Cloud agent working on a database change,<00:09:45.760> whatever,<00:09:46.720> and<00:09:47.080> then<00:09:47.240> you<00:09:47.320> just
[00:09:47] change, whatever, and then you just
[00:09:47] change, whatever, and then you just combine<00:09:48.000> the<00:09:48.080> changes,<00:09:48.560> and<00:09:48.720> you<00:09:48.840> have<00:09:49.120> them
[00:09:49] combine the changes, and you have them
[00:09:49] combine the changes, and you have them running<00:09:49.800> simultaneously<00:09:50.600> because<00:09:50.840> you<00:09:50.920> don't
[00:09:51] running simultaneously because you don't
[00:09:51] running simultaneously because you don't want<00:09:51.360> to<00:09:51.440> be<00:09:51.560> waiting<00:09:51.920> for<00:09:52.120> every<00:09:52.360> single
[00:09:52] want to be waiting for every single
[00:09:52] want to be waiting for every single thing<00:09:52.840> to<00:09:52.920> be<00:09:53.040> finished<00:09:53.400> before<00:09:53.640> you<00:09:53.680> can
[00:09:53] thing to be finished before you can
[00:09:53] thing to be finished before you can start<00:09:54.080> with<00:09:54.200> the<00:09:54.320> next<00:09:54.600> thing.<00:09:55.120> So,<00:09:55.200> you're
[00:09:55] start with the next thing. So, you're
[00:09:55] start with the next thing. So, you're just<00:09:55.560> orchestrating<00:09:56.200> these<00:09:56.560> cloud<00:09:57.280> code
[00:09:57] just orchestrating these cloud code
[00:09:57] just orchestrating these cloud code instances.
[00:09:59] instances.
[00:09:59] instances. Uh,<00:09:59.400> but<00:09:59.520> this<00:09:59.720> was<00:09:59.920> now<00:10:00.120> a<00:10:00.200> very<00:10:00.400> simple
[00:10:00] Uh, but this was now a very simple
[00:10:00] Uh, but this was now a very simple example.<00:10:01.360> Now,<00:10:01.600> if<00:10:01.800> I<00:10:01.880> try<00:10:02.120> to<00:10:02.320> exit<00:10:02.680> here,<00:10:02.800> you
[00:10:02] example. Now, if I try to exit here, you
[00:10:02] example. Now, if I try to exit here, you can<00:10:03.040> see<00:10:03.160> it<00:10:03.320> asks<00:10:03.640> me,<00:10:03.720> do<00:10:03.800> I<00:10:03.880> want<00:10:04.000> to<00:10:04.080> keep
[00:10:04] can see it asks me, do I want to keep
[00:10:04] can see it asks me, do I want to keep the<00:10:04.480> work<00:10:04.680> tree<00:10:04.920> or<00:10:05.040> do<00:10:05.160> I<00:10:05.240> want<00:10:05.400> to<00:10:05.440> remove<00:10:05.760> it?
[00:10:05] the work tree or do I want to remove it?
[00:10:05] the work tree or do I want to remove it? Of<00:10:06.080> course,<00:10:06.280> I<00:10:06.320> want<00:10:06.480> to<00:10:06.560> keep<00:10:06.880> it.<00:10:07.680> And<00:10:07.760> I'm
[00:10:07] Of course, I want to keep it. And I'm
[00:10:07] Of course, I want to keep it. And I'm going<00:10:07.960> to<00:10:08.000> also<00:10:08.240> do<00:10:08.360> the<00:10:08.440> same<00:10:08.680> thing<00:10:08.840> here.
[00:10:09] going to also do the same thing here.
[00:10:09] going to also do the same thing here. I'm<00:10:09.400> going<00:10:09.520> to<00:10:09.600> keep<00:10:09.880> the<00:10:10.000> work<00:10:10.240> tree.
[00:10:11] I'm going to keep the work tree.
[00:10:12] I'm going to keep the work tree. And<00:10:12.400> now<00:10:12.560> I<00:10:12.600> can<00:10:12.760> go<00:10:13.080> to<00:10:13.560> dot<00:10:13.880> cloud
[00:10:15] And now I can go to dot cloud
[00:10:15] And now I can go to dot cloud and<00:10:15.240> do<00:10:15.360> the<00:10:15.440> same<00:10:15.680> thing<00:10:15.840> here<00:10:16.080> as<00:10:16.240> well,<00:10:16.680> dot
[00:10:16] and do the same thing here as well, dot
[00:10:16] and do the same thing here as well, dot cloud.
[00:10:18] cloud.
[00:10:18] cloud. And<00:10:18.240> I<00:10:18.280> can<00:10:18.440> navigate<00:10:18.840> to<00:10:18.960> work<00:10:19.160> trees,<00:10:19.520> and
[00:10:19] And I can navigate to work trees, and
[00:10:19] And I can navigate to work trees, and here<00:10:19.840> I<00:10:19.880> can<00:10:20.040> see<00:10:20.360> I<00:10:20.440> have<00:10:21.400> these<00:10:21.680> work<00:10:21.880> trees
[00:10:22] here I can see I have these work trees
[00:10:22] here I can see I have these work trees on<00:10:22.360> both<00:10:22.680> sides.<00:10:23.120> I<00:10:23.200> have<00:10:23.520> the<00:10:23.640> scriptic
[00:10:24] on both sides. I have the scriptic
[00:10:24] on both sides. I have the scriptic toasting<00:10:24.560> storm<00:10:24.880> and<00:10:25.000> the<00:10:25.040> green<00:10:25.320> theme.<00:10:26.000> I
[00:10:26] toasting storm and the green theme. I
[00:10:26] toasting storm and the green theme. I can<00:10:26.200> go<00:10:26.360> to<00:10:26.480> cryptic<00:10:26.920> toasting<00:10:27.320> storm.<00:10:27.920> First
[00:10:28] can go to cryptic toasting storm. First
[00:10:28] can go to cryptic toasting storm. First of<00:10:28.280> all,<00:10:28.440> I<00:10:28.480> can<00:10:28.600> just<00:10:28.840> use<00:10:29.040> the<00:10:29.160> application
[00:10:29] of all, I can just use the application
[00:10:29] of all, I can just use the application here.<00:10:29.960> I<00:10:30.000> can<00:10:30.160> run<00:10:30.320> it<00:10:30.440> locally,<00:10:30.840> see<00:10:31.000> what<00:10:31.120> it
[00:10:31] here. I can run it locally, see what it
[00:10:31] here. I can run it locally, see what it looks<00:10:31.480> like.<00:10:31.720> For<00:10:31.800> example,<00:10:32.200> here<00:10:32.440> flask<00:10:32.760> to
[00:10:32] looks like. For example, here flask to
[00:10:32] looks like. For example, here flask to do<00:10:33.040> app,<00:10:33.240> UV<00:10:33.480> run<00:10:34.080> main.py.<00:10:34.760> This<00:10:34.960> is<00:10:35.080> now<00:10:35.800> the
[00:10:35] do app, UV run main.py. This is now the
[00:10:36] do app, UV run main.py. This is now the one<00:10:36.520> branch<00:10:36.920> here,<00:10:37.120> the<00:10:37.520> the<00:10:37.600> first<00:10:37.840> one<00:10:37.960> which
[00:10:38] one branch here, the the first one which
[00:10:38] one branch here, the the first one which um<00:10:39.200> was<00:10:39.360> instructed<00:10:39.760> to<00:10:39.840> do<00:10:39.960> everything<00:10:40.400> in<00:10:40.520> a
[00:10:40] um was instructed to do everything in a
[00:10:40] um was instructed to do everything in a red<00:10:40.840> way.<00:10:41.280> There<00:10:41.400> you<00:10:41.480> go,<00:10:42.040> red<00:10:42.280> styling.<00:10:43.280> And
[00:10:43] red way. There you go, red styling. And
[00:10:43] red way. There you go, red styling. And if<00:10:43.560> I<00:10:43.640> go<00:10:43.880> to<00:10:44.160> the<00:10:44.280> green<00:10:44.560> theme,<00:10:45.480> I<00:10:45.560> can<00:10:45.720> go
[00:10:46] if I go to the green theme, I can go
[00:10:46] if I go to the green theme, I can go into<00:10:46.680> flask<00:10:47.040> as<00:10:47.160> well,<00:10:47.480> UV<00:10:47.720> run.
[00:10:49] into flask as well, UV run.
[00:10:49] into flask as well, UV run. First,<00:10:49.560> let<00:10:49.760> me<00:10:49.880> stop<00:10:50.160> this<00:10:50.400> one,<00:10:50.600> UV<00:10:50.800> run
[00:10:50] First, let me stop this one, UV run
[00:10:51] First, let me stop this one, UV run main.py.<00:10:52.120> And<00:10:52.240> then<00:10:52.400> if<00:10:52.520> I<00:10:52.600> open<00:10:52.840> this,<00:10:53.040> it
[00:10:53] main.py. And then if I open this, it
[00:10:53] main.py. And then if I open this, it should<00:10:53.280> be<00:10:53.440> green.
[00:10:54] should be green.
[00:10:55] should be green. There<00:10:55.120> you<00:10:55.200> go,<00:10:55.480> everything's<00:10:55.880> green.<00:10:56.720> So,
[00:10:56] There you go, everything's green. So,
[00:10:56] There you go, everything's green. So, that<00:10:57.080> is<00:10:57.200> one<00:10:57.400> thing.<00:10:57.600> You<00:10:57.720> can<00:10:57.960> try<00:10:58.280> different
[00:10:58] that is one thing. You can try different
[00:10:58] that is one thing. You can try different things,<00:10:59.120> see<00:10:59.520> which<00:10:59.760> one<00:10:59.880> you<00:10:59.960> like<00:11:00.240> more,<00:11:00.920> or
[00:11:01] things, see which one you like more, or
[00:11:01] things, see which one you like more, or you<00:11:01.240> can<00:11:01.360> just<00:11:01.560> combine<00:11:01.960> these.<00:11:02.160> So,<00:11:02.240> I<00:11:02.320> can<00:11:02.480> go
[00:11:02] you can just combine these. So, I can go
[00:11:02] you can just combine these. So, I can go now.<00:11:03.240> As<00:11:03.400> you<00:11:03.480> can<00:11:03.640> see<00:11:03.880> here,<00:11:04.360> maybe<00:11:04.520> let<00:11:04.680> me
[00:11:04] now. As you can see here, maybe let me
[00:11:04] now. As you can see here, maybe let me clear<00:11:05.040> the
[00:11:05] clear the
[00:11:05] clear the the<00:11:05.760> screen<00:11:06.080> so<00:11:06.240> it's<00:11:06.960> easier<00:11:07.280> to<00:11:07.480> read.<00:11:08.000> But
[00:11:08] the screen so it's easier to read. But
[00:11:08] the screen so it's easier to read. But basically,<00:11:08.560> we<00:11:08.720> have<00:11:09.000> here<00:11:09.440> the<00:11:09.600> name<00:11:09.960> of<00:11:10.120> the
[00:11:10] basically, we have here the name of the
[00:11:10] basically, we have here the name of the branch<00:11:10.680> or<00:11:10.839> of<00:11:10.960> the<00:11:11.040> work<00:11:11.240> tree,<00:11:11.400> and<00:11:11.480> then<00:11:11.600> you
[00:11:11] branch or of the work tree, and then you
[00:11:11] branch or of the work tree, and then you have<00:11:12.040> the<00:11:12.280> upstream<00:11:12.760> branch<00:11:13.200> branch.<00:11:13.600> So,
[00:11:13] have the upstream branch branch. So,
[00:11:13] have the upstream branch branch. So, this<00:11:13.960> is<00:11:14.080> actually<00:11:14.400> pushing<00:11:14.800> to<00:11:14.920> main.<00:11:15.160> If<00:11:15.320> I
[00:11:15] this is actually pushing to main. If I
[00:11:15] this is actually pushing to main. If I push<00:11:15.680> this,<00:11:15.920> it's<00:11:16.040> going<00:11:16.200> to<00:11:16.280> push<00:11:16.520> to<00:11:16.640> main.
[00:11:16] push this, it's going to push to main.
[00:11:17] push this, it's going to push to main. Now,<00:11:17.120> if<00:11:17.240> I<00:11:17.280> do<00:11:17.520> want<00:11:17.720> to<00:11:17.839> push<00:11:18.160> to<00:11:18.320> this<00:11:18.640> actual
[00:11:19] Now, if I do want to push to this actual
[00:11:19] Now, if I do want to push to this actual branch,<00:11:19.400> what<00:11:19.520> I<00:11:19.600> can<00:11:19.760> do<00:11:19.960> is<00:11:20.120> first<00:11:20.360> of<00:11:20.440> all,<00:11:20.520> I
[00:11:20] branch, what I can do is first of all, I
[00:11:20] branch, what I can do is first of all, I need<00:11:20.720> to<00:11:20.800> say<00:11:21.000> get<00:11:21.160> status.<00:11:21.720> I<00:11:21.760> can<00:11:21.920> say,<00:11:22.200> okay,
[00:11:22] need to say get status. I can say, okay,
[00:11:22] need to say get status. I can say, okay, what<00:11:23.080> did<00:11:23.360> actually<00:11:23.680> change?<00:11:24.120> It<00:11:24.280> changed<00:11:24.640> the
[00:11:24] what did actually change? It changed the
[00:11:24] what did actually change? It changed the style.css.<00:11:25.560> So,<00:11:25.760> app<00:11:26.240> static
[00:11:27] style.css. So, app static
[00:11:27] style.css. So, app static and<00:11:28.000> then<00:11:28.600> CSS<00:11:29.200> style.css.<00:11:30.480> And<00:11:30.920> now<00:11:31.080> what<00:11:31.200> I
[00:11:31] and then CSS style.css. And now what I
[00:11:31] and then CSS style.css. And now what I can<00:11:31.400> do<00:11:31.520> is<00:11:31.600> I<00:11:31.640> can<00:11:31.800> say,<00:11:32.520> get<00:11:33.200> commit<00:11:33.839> first<00:11:34.120> of
[00:11:34] can do is I can say, get commit first of
[00:11:34] can do is I can say, get commit first of all.
[00:11:36] all.
[00:11:36] all. Red<00:11:36.880> style,<00:11:37.520> and<00:11:37.960> then<00:11:38.600> now<00:11:38.920> there's<00:11:39.120> a<00:11:39.160> typo,
[00:11:39] Red style, and then now there's a typo,
[00:11:39] Red style, and then now there's a typo, red<00:11:39.640> style<00:11:39.839> four,<00:11:40.240> doesn't<00:11:40.440> matter.<00:11:41.160> Get
[00:11:41] red style four, doesn't matter. Get
[00:11:41] red style four, doesn't matter. Get push,<00:11:41.760> and<00:11:41.839> now<00:11:42.000> I<00:11:42.080> would<00:11:42.280> say<00:11:42.720> origin<00:11:43.800> head.
[00:11:44] push, and now I would say origin head.
[00:11:44] push, and now I would say origin head. So,<00:11:44.480> that<00:11:44.720> would<00:11:44.880> push<00:11:45.160> it<00:11:45.320> to<00:11:45.480> the<00:11:45.600> work<00:11:45.839> tree
[00:11:46] So, that would push it to the work tree
[00:11:46] So, that would push it to the work tree cryptic<00:11:46.640> toasting<00:11:47.320> storm.<00:11:47.760> If<00:11:47.880> I<00:11:47.920> want<00:11:48.080> to
[00:11:48] cryptic toasting storm. If I want to
[00:11:48] cryptic toasting storm. If I want to push<00:11:48.400> to<00:11:48.520> main<00:11:48.760> explicitly,<00:11:49.360> I<00:11:49.440> would<00:11:49.560> do<00:11:49.800> head
[00:11:50] push to main explicitly, I would do head
[00:11:50] push to main explicitly, I would do head colon<00:11:50.720> main.<00:11:51.520> But<00:11:51.640> I'm<00:11:51.720> just<00:11:51.880> going<00:11:52.000> to<00:11:52.040> go
[00:11:52] colon main. But I'm just going to go
[00:11:52] colon main. But I'm just going to go with<00:11:52.360> head<00:11:53.040> like<00:11:53.240> this.
[00:11:55] with head like this.
[00:11:55] with head like this. Pushed<00:11:55.360> it<00:11:55.640> to<00:11:55.760> a<00:11:55.800> new<00:11:55.960> branch.<00:11:56.480> Now,<00:11:56.680> also,
[00:11:57] Pushed it to a new branch. Now, also,
[00:11:57] Pushed it to a new branch. Now, also, one<00:11:57.360> thing<00:11:57.480> that<00:11:57.600> you<00:11:57.680> will<00:11:57.760> notice<00:11:58.040> is<00:11:58.160> you
[00:11:58] one thing that you will notice is you
[00:11:58] one thing that you will notice is you have<00:11:58.440> to<00:11:58.520> do<00:11:58.720> it<00:11:58.839> like<00:11:59.040> this<00:11:59.240> every<00:11:59.480> time
[00:11:59] have to do it like this every time
[00:11:59] have to do it like this every time because<00:11:59.960> it's<00:12:00.120> pointing<00:12:00.720> upstream<00:12:01.200> to<00:12:01.400> main.
[00:12:01] because it's pointing upstream to main.
[00:12:01] because it's pointing upstream to main. If<00:12:01.960> I<00:12:02.040> don't<00:12:02.400> want<00:12:02.600> this,<00:12:02.800> if<00:12:02.920> I<00:12:03.000> actually<00:12:03.360> want
[00:12:03] If I don't want this, if I actually want
[00:12:03] If I don't want this, if I actually want to<00:12:04.560> um<00:12:05.160> to<00:12:05.640> to<00:12:05.760> always<00:12:06.080> push<00:12:06.480> to<00:12:06.880> this<00:12:07.120> here,<00:12:07.360> I
[00:12:07] to um to to always push to this here, I
[00:12:07] to um to to always push to this here, I can<00:12:07.720> unset<00:12:08.520> the<00:12:08.640> upstream.<00:12:09.160> So,<00:12:09.280> I<00:12:09.360> can<00:12:09.480> say,
[00:12:09] can unset the upstream. So, I can say,
[00:12:09] can unset the upstream. So, I can say, get<00:12:10.360> branch<00:12:11.200> {dash}<00:12:11.520> {dash}<00:12:12.400> unset<00:12:13.360> {dash}
[00:12:13] get branch {dash} {dash} unset {dash}
[00:12:13] get branch {dash} {dash} unset {dash} upstream.
[00:12:15] upstream.
[00:12:15] upstream. And<00:12:16.040> now<00:12:16.680> this<00:12:16.880> is<00:12:17.000> no<00:12:17.120> longer<00:12:17.360> the<00:12:17.480> case.
[00:12:17] And now this is no longer the case.
[00:12:17] And now this is no longer the case. We're<00:12:17.960> pushing<00:12:18.320> to<00:12:18.480> upstream<00:12:19.280> work<00:12:19.520> tree
[00:12:19] We're pushing to upstream work tree
[00:12:19] We're pushing to upstream work tree cryptic<00:12:20.240> toasting<00:12:20.560> storm.<00:12:21.160> Yeah,<00:12:21.320> so<00:12:21.480> to
[00:12:21] cryptic toasting storm. Yeah, so to
[00:12:21] cryptic toasting storm. Yeah, so to summarize,<00:12:22.000> the<00:12:22.080> basic<00:12:22.360> idea<00:12:22.640> is<00:12:22.800> a<00:12:22.880> work<00:12:23.120> tree
[00:12:23] summarize, the basic idea is a work tree
[00:12:23] summarize, the basic idea is a work tree in<00:12:23.520> Git<00:12:23.800> is<00:12:24.040> just<00:12:24.480> different<00:12:24.800> work<00:12:25.040> trees
[00:12:25] in Git is just different work trees
[00:12:25] in Git is just different work trees having<00:12:25.839> different<00:12:26.200> branches<00:12:26.640> of<00:12:26.800> your
[00:12:26] having different branches of your
[00:12:26] having different branches of your repository<00:12:27.560> checked<00:12:27.920> out<00:12:28.160> locally<00:12:28.560> at<00:12:28.680> the
[00:12:28] repository checked out locally at the
[00:12:28] repository checked out locally at the same<00:12:29.040> time.<00:12:29.800> You<00:12:29.920> can<00:12:30.120> use<00:12:30.280> that<00:12:30.520> manually<00:12:30.960> to
[00:12:31] same time. You can use that manually to
[00:12:31] same time. You can use that manually to do<00:12:31.240> stuff.<00:12:31.560> You<00:12:31.640> can<00:12:31.760> also<00:12:32.040> have<00:12:32.440> agents<00:12:32.920> use
[00:12:33] do stuff. You can also have agents use
[00:12:33] do stuff. You can also have agents use that.<00:12:33.360> You<00:12:33.440> can<00:12:33.640> either<00:12:33.880> create<00:12:34.160> them
[00:12:34] that. You can either create them
[00:12:34] that. You can either create them manually<00:12:34.800> and<00:12:34.920> run<00:12:35.080> something<00:12:35.360> like<00:12:35.560> open
[00:12:35] manually and run something like open
[00:12:35] manually and run something like open code<00:12:36.040> in<00:12:36.200> every<00:12:36.480> single<00:12:36.760> directory<00:12:37.280> of<00:12:37.400> the
[00:12:37] code in every single directory of the
[00:12:37] code in every single directory of the work<00:12:37.760> trees<00:12:38.000> you<00:12:38.120> create,<00:12:38.920> or<00:12:39.080> you<00:12:39.160> can<00:12:39.280> just
[00:12:39] work trees you create, or you can just
[00:12:39] work trees you create, or you can just use<00:12:39.760> cloud<00:12:40.080> code<00:12:40.440> {dash}<00:12:40.760> {dash}<00:12:41.040> work<00:12:41.280> tree
[00:12:41] use cloud code {dash} {dash} work tree
[00:12:41] use cloud code {dash} {dash} work tree and<00:12:41.520> then<00:12:41.640> give<00:12:41.800> it<00:12:41.880> a<00:12:41.920> name,<00:12:42.240> and<00:12:42.360> it<00:12:42.400> will
[00:12:42] and then give it a name, and it will
[00:12:42] and then give it a name, and it will automatically<00:12:43.480> handle<00:12:43.800> the<00:12:43.920> work<00:12:44.120> trees.<00:12:44.360> You
[00:12:44] automatically handle the work trees. You
[00:12:44] automatically handle the work trees. You just<00:12:44.720> need<00:12:44.920> to<00:12:45.000> do<00:12:45.160> the<00:12:45.280> commits<00:12:45.640> and<00:12:45.760> the
[00:12:45] just need to do the commits and the
[00:12:45] just need to do the commits and the pushes,<00:12:46.560> or<00:12:46.720> you<00:12:46.760> can<00:12:46.920> even<00:12:47.120> ask<00:12:47.440> cloud<00:12:47.720> to<00:12:47.839> do
[00:12:47] pushes, or you can even ask cloud to do
[00:12:47] pushes, or you can even ask cloud to do that<00:12:48.160> for<00:12:48.400> you.<00:12:48.680> So,<00:12:48.880> that's<00:12:49.160> it<00:12:49.240> for<00:12:49.360> this
[00:12:49] that for you. So, that's it for this
[00:12:49] that for you. So, that's it for this video<00:12:49.680> today.<00:12:50.040> I<00:12:50.160> hope<00:12:50.440> you<00:12:50.520> enjoyed<00:12:50.880> it<00:12:51.000> and
[00:12:51] video today. I hope you enjoyed it and
[00:12:51] video today. I hope you enjoyed it and hope<00:12:51.240> you<00:12:51.320> learned<00:12:51.560> something.<00:12:52.040> If<00:12:52.200> so,<00:12:52.400> let
[00:12:52] hope you learned something. If so, let
[00:12:52] hope you learned something. If so, let me<00:12:52.640> know<00:12:52.800> by<00:12:52.960> hitting<00:12:53.240> a<00:12:53.280> like<00:12:53.480> button<00:12:53.720> and
[00:12:53] me know by hitting a like button and
[00:12:53] me know by hitting a like button and leave<00:12:53.960> a<00:12:54.040> comment<00:12:54.400> in<00:12:54.440> the<00:12:54.520> comment<00:12:54.800> section
[00:12:55] leave a comment in the comment section
[00:12:55] leave a comment in the comment section down<00:12:55.320> below.<00:12:56.000> Also,<00:12:56.480> if<00:12:56.680> you're<00:12:56.839> interested,
[00:12:57] down below. Also, if you're interested,
[00:12:57] down below. Also, if you're interested, on<00:12:57.440> my<00:12:57.560> website<00:12:57.920> you<00:12:57.960> will<00:12:58.080> find<00:12:58.320> a<00:12:58.360> services
[00:12:58] on my website you will find a services
[00:12:58] on my website you will find a services tab<00:12:59.080> and<00:12:59.240> a<00:12:59.320> tutoring<00:12:59.800> tab.<00:13:00.160> There<00:13:00.280> you<00:13:00.400> can
[00:13:00] tab and a tutoring tab. There you can
[00:13:00] tab and a tutoring tab. There you can see<00:13:00.680> what<00:13:00.800> I<00:13:00.839> have<00:13:01.040> to<00:13:01.160> offer.<00:13:01.480> If<00:13:01.600> you<00:13:01.640> need
[00:13:01] see what I have to offer. If you need
[00:13:01] see what I have to offer. If you need help<00:13:02.040> with<00:13:02.200> a<00:13:02.240> project,<00:13:02.760> if<00:13:02.880> you<00:13:02.920> need<00:13:03.080> some
[00:13:03] help with a project, if you need some
[00:13:03] help with a project, if you need some consulting,<00:13:03.920> or<00:13:04.040> if<00:13:04.160> you<00:13:04.240> want<00:13:04.480> me<00:13:04.600> to<00:13:04.720> teach
[00:13:04] consulting, or if you want me to teach
[00:13:04] consulting, or if you want me to teach you<00:13:05.040> something<00:13:05.400> one-on-one,<00:13:06.160> you<00:13:06.280> can
[00:13:06] you something one-on-one, you can
[00:13:06] you something one-on-one, you can contact<00:13:06.800> me<00:13:06.920> there<00:13:07.160> via<00:13:07.440> email<00:13:07.920> or<00:13:08.080> LinkedIn
[00:13:08] contact me there via email or LinkedIn
[00:13:08] contact me there via email or LinkedIn at<00:13:08.560> the<00:13:08.640> bottom<00:13:09.040> of<00:13:09.160> the<00:13:09.280> page<00:13:09.960> down<00:13:10.200> below.
[00:13:10] at the bottom of the page down below.
[00:13:10] at the bottom of the page down below. Besides<00:13:11.200> that,<00:13:11.400> don't<00:13:11.560> forget<00:13:11.800> to<00:13:11.880> subscribe
[00:13:12] Besides that, don't forget to subscribe
[00:13:12] Besides that, don't forget to subscribe to<00:13:12.360> this<00:13:12.480> channel<00:13:12.880> and<00:13:13.080> hit<00:13:13.240> the<00:13:13.320> notification
[00:13:13] to this channel and hit the notification
[00:13:13] to this channel and hit the notification bell<00:13:14.080> to<00:13:14.160> not<00:13:14.360> miss<00:13:14.520> a<00:13:14.560> single<00:13:14.880> future<00:13:15.240> video
[00:13:15] bell to not miss a single future video
[00:13:15] bell to not miss a single future video for<00:13:15.680> free.<00:13:16.240> Other<00:13:16.440> than<00:13:16.520> that,<00:13:16.839> thank<00:13:16.960> you
[00:13:16] for free. Other than that, thank you
[00:13:17] for free. Other than that, thank you very<00:13:17.120> much<00:13:17.200> for<00:13:17.280> watching.<00:13:17.720> See<00:13:17.839> you<00:13:17.880> in<00:13:17.960> the
[00:13:17] very much for watching. See you in the
[00:13:18] very much for watching. See you in the next<00:13:18.240> video,<00:13:18.839> and<00:13:19.320> bye.

---

*Source: [https://www.youtube.com/watch?v=3ntrfMSMNVc](https://www.youtube.com/watch?v=3ntrfMSMNVc)*
